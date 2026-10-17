//! `JobTable`: the job table on a caller's connection or transaction, a caller-named table with
//! extra unique constraints, the due-time query and the revision compare-and-swap.

use std::sync::Arc;

use rightkit_job_queue::rusqlite::Connection;
use rightkit_job_queue::{
    Expect, Ext, ExtColumn, JobError, JobRecord, JobResult, JobStore, JobTable, NewJob, Resolution,
    StatusModel, StoreConfig, StoreHook, Submission, SubmitOptions, Transition, FAILED,
    REVISION_CONFLICT,
};
use serde_json::json;

/// An app ledger that writes in the same transaction as each new job.
struct Quota;

impl StoreHook for Quota {
    fn on_create(&self, c: &Connection, j: &JobRecord) -> JobResult<()> {
        c.execute(
            "INSERT INTO quota (venture, used) VALUES (?1, 1) \
             ON CONFLICT(venture) DO UPDATE SET used = used + 1",
            [j.ext_str("venture").unwrap_or("")],
        )?;
        Ok(())
    }
}

fn cfg(hook: bool) -> StoreConfig {
    let model = StatusModel::new("queued", ["queued", "publishing", "held"])
        .allow("queued", "publishing")
        .run_status(Some("publishing"))
        .hold("held");
    let mut c = StoreConfig::new()
        .jobs_table("publish_jobs")
        .column(ExtColumn::text("venture").not_null())
        .column(ExtColumn::text("channel").not_null())
        .column(ExtColumn::text("entry_id").not_null())
        .column(ExtColumn::text("provider_id"))
        .column(ExtColumn::text("next_attempt_at"))
        .unique(["venture", "channel", "entry_id"])
        .row_revision("revision")
        .due_column("next_attempt_at")
        .external_id("provider_id")
        .statuses(model)
        .app_schema("CREATE TABLE IF NOT EXISTS quota (venture TEXT PRIMARY KEY, used INTEGER NOT NULL);")
        .app_schema(
            "CREATE TABLE IF NOT EXISTS post_log (job_id TEXT NOT NULL REFERENCES publish_jobs(job_id), note TEXT NOT NULL);",
        );
    if hook {
        c = c.hook(Arc::new(Quota));
    }
    c
}

fn open(hook: bool) -> (tempfile::TempDir, Connection, JobTable) {
    let dir = tempfile::tempdir().unwrap();
    let conn = Connection::open(dir.path().join("app.sqlite")).unwrap();
    let table = JobTable::new(&cfg(hook)).unwrap();
    table.init_schema(&conn).unwrap();
    (dir, conn, table)
}

fn job(rid: &str) -> NewJob {
    NewJob {
        request_id: Some(rid.to_string()),
        kind: "publish".into(),
        request: json!({"rid": rid}),
    }
}

fn ext(venture: &str, channel: &str, entry: &str, next: Option<&str>) -> Ext {
    let mut x = Ext::new();
    x.insert("venture".into(), json!(venture));
    x.insert("channel".into(), json!(channel));
    x.insert("entry_id".into(), json!(entry));
    if let Some(n) = next {
        x.insert("next_attempt_at".into(), json!(n));
    }
    x
}

fn add(
    table: &JobTable,
    conn: &Connection,
    rid: &str,
    entry: &str,
    next: Option<&str>,
) -> JobResult<JobRecord> {
    table
        .enqueue(
            conn,
            &job(rid),
            &ext("v", "bluesky", entry, next),
            &SubmitOptions::default(),
        )
        .map(|s| s.record)
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn named_table_with_extra_unique_constraint() {
    let (_d, conn, table) = open(false);
    let tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='jobs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0, "the default table is not created");
    assert_eq!(table.name(), "publish_jobs");

    add(&table, &conn, "r1", "e1", None).unwrap();
    add(&table, &conn, "r2", "e2", None).unwrap();
    // Same (venture, channel, entry_id) under another request id: the unique constraint refuses.
    assert!(add(&table, &conn, "r3", "e1", None).is_err());
    assert_eq!(count(&conn, "publish_jobs"), 2);
    // Same request id: deduplicated, not an error.
    let again = table
        .enqueue(
            &conn,
            &job("r1"),
            &ext("v", "bluesky", "e1", None),
            &SubmitOptions::default(),
        )
        .unwrap();
    assert!(again.deduplicated);
    // Schema init is idempotent.
    table.init_schema(&conn).unwrap();
    assert_eq!(count(&conn, "publish_jobs"), 2);
}

#[test]
fn caller_transaction_rolls_back_job_and_app_row() {
    let (_d, mut conn, table) = open(false);
    {
        let tx = conn.transaction().unwrap();
        let rec = add(&table, &tx, "r1", "e1", None).unwrap();
        let id = rec.job.job_id.clone();
        table
            .transition(&tx, &id, Expect::new(), &Transition::to("publishing"))
            .unwrap()
            .unwrap();
        tx.execute("INSERT INTO quota (venture, used) VALUES ('v', 5)", [])
            .unwrap();
        tx.execute(
            "INSERT INTO post_log (job_id, note) VALUES (?1, 'queued')",
            [&id],
        )
        .unwrap();
        assert_eq!(count(&tx, "publish_jobs"), 1);
        // Dropped without commit: everything goes.
    }
    assert_eq!(count(&conn, "publish_jobs"), 0);
    assert_eq!(count(&conn, "quota"), 0);
    assert_eq!(count(&conn, "post_log"), 0);
    assert_eq!(count(&conn, "job_events"), 0);

    {
        let tx = conn.transaction().unwrap();
        let rec = add(&table, &tx, "r1", "e1", None).unwrap();
        assert_eq!(rec.job.status, "queued");
        table
            .transition(
                &tx,
                &rec.job.job_id,
                Expect::new(),
                &Transition::to("publishing"),
            )
            .unwrap()
            .unwrap();
        tx.execute("INSERT INTO quota (venture, used) VALUES ('v', 5)", [])
            .unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(count(&conn, "publish_jobs"), 1);
    assert_eq!(count(&conn, "quota"), 1);
    let rec = table.find_by_request(&conn, "r1").unwrap().unwrap();
    assert_eq!(rec.job.status, "publishing");
}

#[test]
fn hook_writes_in_the_caller_transaction() {
    let (_d, mut conn, table) = open(true);
    {
        let tx = conn.transaction().unwrap();
        add(&table, &tx, "r1", "e1", None).unwrap();
        let used: i64 = tx
            .query_row("SELECT used FROM quota WHERE venture='v'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(used, 1, "the hook ran on the caller's transaction");
    }
    assert_eq!(count(&conn, "quota"), 0);
    assert_eq!(count(&conn, "publish_jobs"), 0);

    let tx = conn.transaction().unwrap();
    add(&table, &tx, "r1", "e1", None).unwrap();
    add(&table, &tx, "r2", "e2", None).unwrap();
    tx.commit().unwrap();
    let used: i64 = conn
        .query_row("SELECT used FROM quota WHERE venture='v'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(used, 2);
}

#[test]
fn complete_fail_hold_and_resolve_on_a_caller_connection() {
    let (_d, mut conn, table) = open(false);

    let a = add(&table, &conn, "r1", "e1", None).unwrap();
    assert!(table
        .complete(&conn, &a.job.job_id, Expect::new(), &json!({"ok": true}))
        .unwrap());
    // Already terminal: nothing written.
    assert!(!table
        .complete(&conn, &a.job.job_id, Expect::new(), &json!({"ok": false}))
        .unwrap());
    assert_eq!(table.get(&conn, &a.job.job_id).unwrap().status, "complete");

    let b = add(&table, &conn, "r2", "e2", None).unwrap();
    assert!(table
        .fail(
            &conn,
            &b.job.job_id,
            Expect::new(),
            &JobError::new("boom", "failed")
        )
        .unwrap());
    assert_eq!(table.get(&conn, &b.job.job_id).unwrap().status, FAILED);

    let c = add(&table, &conn, "r3", "e3", None).unwrap();
    let id = c.job.job_id.clone();
    let sub = table
        .begin_submission(&conn, &id, Expect::new(), 3)
        .unwrap()
        .unwrap();
    assert!(matches!(
        sub,
        Submission::Submit {
            attempt: 1,
            replay: false,
            ..
        }
    ));

    // A hold that rolls back leaves the job where it was.
    {
        let tx = conn.transaction().unwrap();
        let held = table
            .hold_submission(
                &tx,
                &id,
                Expect::new(),
                &JobError::new("unknown", "outcome unknown"),
                "held",
            )
            .unwrap()
            .unwrap();
        assert_eq!(held.job.status, "held");
    }
    assert_eq!(table.get(&conn, &id).unwrap().status, "queued");

    {
        let tx = conn.transaction().unwrap();
        table
            .hold_submission(
                &tx,
                &id,
                Expect::new(),
                &JobError::new("unknown", "outcome unknown"),
                "held",
            )
            .unwrap()
            .unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(table.get(&conn, &id).unwrap().status, "held");
    // A held job leaves only through resolve.
    let err = table
        .transition(&conn, &id, Expect::new(), &Transition::to("publishing"))
        .unwrap_err();
    assert_eq!(err.code, "illegal_transition");

    let tx = conn.transaction().unwrap();
    let done = table
        .resolve(
            &tx,
            &id,
            Expect::new(),
            &Resolution::Abandon(JobError::new("gave_up", "not published")),
            None,
        )
        .unwrap();
    tx.commit().unwrap();
    assert_eq!(done.job.status, FAILED);
}

#[test]
fn due_query_orders_and_filters_with_injected_now() {
    let (_d, conn, table) = open(false);
    add(&table, &conn, "r1", "late", Some("2026-01-03T00:00:00Z")).unwrap();
    add(&table, &conn, "r2", "early", Some("2026-01-01T00:00:00Z")).unwrap();
    add(&table, &conn, "r3", "future", Some("2026-03-01T00:00:00Z")).unwrap();
    add(&table, &conn, "r4", "unset", None).unwrap();
    let done = add(&table, &conn, "r5", "done", Some("2026-01-02T00:00:00Z")).unwrap();
    table
        .complete(&conn, &done.job.job_id, Expect::new(), &json!({}))
        .unwrap();

    let now = json!("2026-01-31T00:00:00Z");
    let due = table.due(&conn, &now, 10).unwrap();
    let ids: Vec<&str> = due.iter().map(|r| r.ext_str("entry_id").unwrap()).collect();
    assert_eq!(ids, ["early", "late"]);

    let one = table.due(&conn, &now, 1).unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].ext_str("entry_id"), Some("early"));

    // Nothing is due before the earliest time.
    assert!(table
        .due(&conn, &json!("2025-12-31T00:00:00Z"), 10)
        .unwrap()
        .is_empty());

    assert_eq!(
        table
            .due_with(&conn, &now, 10, Some(&["queued"]))
            .unwrap()
            .len(),
        2
    );
    assert!(table
        .due_with(&conn, &now, 10, Some(&["publishing"]))
        .unwrap()
        .is_empty());
    // A held or terminal status is never a runnable filter.
    assert_eq!(
        table
            .due_with(&conn, &now, 10, Some(&["held"]))
            .unwrap_err()
            .code,
        "bad_input"
    );
}

#[test]
fn revision_compare_and_swap_returns_a_typed_conflict() {
    let (_d, conn, table) = open(false);
    let rec = add(&table, &conn, "r1", "e1", None).unwrap();
    let id = rec.job.job_id.clone();
    assert_eq!(table.revision_of(&rec), Some(0));

    let moved = table
        .transition(
            &conn,
            &id,
            Expect::new().revision(0),
            &Transition::to("publishing"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(table.revision_of(&moved), Some(1));

    // A writer that still holds revision 0 is refused and nothing is written.
    let err = table
        .transition(
            &conn,
            &id,
            Expect::new().revision(0),
            &Transition::keep().set("provider_id", "stale"),
        )
        .unwrap_err();
    assert!(err.is_revision_conflict());
    assert_eq!(err.code, REVISION_CONFLICT);
    let now = table.record(&conn, &id).unwrap();
    assert_eq!(now.ext_str("provider_id"), None);
    assert_eq!(table.revision_of(&now), Some(1));

    let next = table
        .transition(
            &conn,
            &id,
            Expect::new().revision(1),
            &Transition::keep().set("provider_id", "p1"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(next.ext_str("provider_id"), Some("p1"));
    assert_eq!(table.revision_of(&next), Some(2));

    // A write without a revision check still bumps it.
    let bumped = table
        .set_ext(&conn, &id, Expect::new(), &Ext::new())
        .unwrap();
    assert_eq!(table.revision_of(&bumped), Some(3));

    // The revision column is the store's own.
    let mut bad = Ext::new();
    bad.insert("revision".into(), json!(99));
    assert_eq!(
        table
            .set_ext(&conn, &id, Expect::new(), &bad)
            .unwrap_err()
            .code,
        "bad_input"
    );
}

#[test]
fn store_and_table_share_the_same_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open_configured(dir.path().join("jobs.sqlite"), cfg(false)).unwrap();
    let (rec, _) = store
        .submit_ext(
            &job("r1"),
            &ext("v", "bluesky", "e1", Some("2026-01-01T00:00:00Z")),
        )
        .unwrap();
    let id = rec.job.job_id.clone();
    store
        .transition(&id, &Transition::to("publishing"))
        .unwrap()
        .unwrap();

    let seen = store.read(|c| store.table().record(c, &id)).unwrap();
    assert_eq!(seen.job.status, "publishing");
    assert_eq!(store.table().revision_of(&seen), Some(1));

    // Due, and in an unheld non-terminal status.
    let due = store.due(&json!("2026-02-01T00:00:00Z"), 5).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].job.job_id, id);

    // A compare-and-swap complete on the store's own connection, through the table.
    let done = store
        .write(|c| {
            store
                .table()
                .complete(c, &id, Expect::new().revision(1), &json!({"ok": true}))
        })
        .unwrap();
    assert!(done);
    assert_eq!(store.get(&id).unwrap().status, "complete");
}

/// A hook that rejects every new job, as a quota ledger over its limit would.
struct Reject;

impl StoreHook for Reject {
    fn on_create(&self, _: &Connection, _: &JobRecord) -> JobResult<()> {
        Err(JobError::new("quota_exceeded", "over quota"))
    }
}

#[test]
fn rejected_on_create_leaves_no_committed_job_on_an_autocommit_connection() {
    let dir = tempfile::tempdir().unwrap();
    let conn = Connection::open(dir.path().join("app.sqlite")).unwrap();
    let table = JobTable::new(&cfg(false).hook(Arc::new(Reject))).unwrap();
    table.init_schema(&conn).unwrap();
    assert!(conn.is_autocommit());

    let err = add(&table, &conn, "r1", "e1", None).unwrap_err();
    assert_eq!(err.code, "quota_exceeded");
    assert_eq!(count(&conn, "publish_jobs"), 0);
    assert!(conn.is_autocommit());
}

#[test]
fn begin_run_fenced_returns_the_current_revision() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open_configured(dir.path().join("jobs.sqlite"), cfg(false)).unwrap();
    let (rec, _) = store
        .submit_ext(&job("r1"), &ext("v", "bluesky", "e1", None))
        .unwrap();
    let id = rec.job.job_id.clone();
    let (run, _) = store.begin_run_fenced(&id, true).unwrap().unwrap();
    let stored = store.read(|c| store.table().record(c, &id)).unwrap();
    assert_eq!(
        store.table().revision_of(&run),
        store.table().revision_of(&stored)
    );
    // The returned revision is usable for a compare-and-swap without a competing writer.
    let rev = store.table().revision_of(&run).unwrap();
    store
        .write(|c| {
            store.table().set_ext(
                c,
                &id,
                Expect::new().revision(rev),
                &ext("v", "bluesky", "e1", None),
            )
        })
        .unwrap();
}
