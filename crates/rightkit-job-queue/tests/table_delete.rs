//! `JobTable::delete_terminal`: a terminal job and its crate-owned side rows go together on the
//! caller's connection; anything not terminal is refused; the caller's rollback restores it all;
//! no file is touched.

use std::path::{Path, PathBuf};

use rightkit_job_queue::rusqlite::Connection;
use rightkit_job_queue::{
    Expect, Ext, ExtColumn, JobError, JobTable, NewJob, Recovery, StatusModel, StoreConfig,
    SubmitOptions, Transition, CANCELLED, REVISION_CONFLICT, STALE_ATTEMPT,
};
use serde_json::json;

fn cfg() -> StoreConfig {
    // `send_failed` is retryable (it can go back to `queued`): it is NOT terminal, whatever its
    // name suggests. Only complete, failed and cancelled are.
    let model = StatusModel::new("queued", ["queued", "sending", "send_failed", "held"])
        .allow("queued", "sending")
        .allow("sending", "send_failed")
        .allow("send_failed", "queued")
        .run_status(Some("sending"))
        .hold("held");
    StoreConfig::new()
        .jobs_table("mail_jobs")
        .column(ExtColumn::text("account").not_null())
        .column(ExtColumn::text("provider_id"))
        .external_id("provider_id")
        .row_revision("revision")
        .app_schema(
            "CREATE TABLE IF NOT EXISTS mail_note \
             (job_id TEXT NOT NULL REFERENCES mail_jobs(job_id), note TEXT NOT NULL);",
        )
        .statuses(model)
}

fn open() -> (tempfile::TempDir, Connection, JobTable) {
    let dir = tempfile::tempdir().unwrap();
    let conn = Connection::open(dir.path().join("app.sqlite")).unwrap();
    let table = JobTable::new(&cfg()).unwrap();
    table.init_schema(&conn).unwrap();
    (dir, conn, table)
}

fn new_job(rid: &str) -> NewJob {
    NewJob {
        request_id: Some(rid.to_string()),
        kind: "mail".into(),
        request: json!({"rid": rid}),
    }
}

/// A queued job with one row in every crate-owned side table; its artifact points at a real
/// file under `dir`. Returns the job id and that file.
fn make_job(table: &JobTable, conn: &Connection, dir: &Path, rid: &str) -> (String, PathBuf) {
    let mut ext = Ext::new();
    ext.insert("account".into(), json!("acct"));
    let rec = table
        .enqueue(
            conn,
            &new_job(rid),
            &ext,
            &SubmitOptions::new().recovery(Recovery::Never),
        )
        .unwrap()
        .record;
    let id = rec.job.job_id;
    table
        .begin_submission(conn, &id, Expect::new(), 3)
        .unwrap()
        .unwrap();
    let file = dir.join(format!("{rid}.eml"));
    std::fs::write(&file, b"body").unwrap();
    conn.execute(
        "INSERT INTO job_events (job_id, at, status, stage, attempt) \
         VALUES (?1, '2026-01-01T00:00:00Z', 'queued', 'test', 0)",
        [&id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO artifacts (artifact_id, job_id, path, sha256, bytes, mime, created_at) \
         VALUES (?1, ?2, ?3, 'abc', 4, 'message/rfc822', '2026-01-01T00:00:00Z')",
        rightkit_job_queue::rusqlite::params![
            format!("art-{rid}"),
            id,
            file.to_string_lossy().to_string()
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO rk_job_deleting (job_id, keep_files, started_at) VALUES (?1, 0, 'x')",
        [&id],
    )
    .unwrap();
    (id, file)
}

/// Row counts keyed by `job_id`: job, events, submissions, artifacts, meta, deleting mark.
fn sides(conn: &Connection, id: &str) -> [i64; 6] {
    let n = |t: &str| -> i64 {
        conn.query_row(
            &format!("SELECT count(*) FROM {t} WHERE job_id=?1"),
            [id],
            |r| r.get(0),
        )
        .unwrap()
    };
    [
        n("mail_jobs"),
        n("job_events"),
        n("rk_job_submissions"),
        n("artifacts"),
        n("rk_job_meta"),
        n("rk_job_deleting"),
    ]
}

fn finish(table: &JobTable, conn: &Connection, id: &str, how: &str) {
    match how {
        "complete" => assert!(table
            .complete(conn, id, Expect::new(), &json!({"ok": true}))
            .unwrap()),
        "failed" => assert!(table
            .fail(conn, id, Expect::new(), &JobError::new("boom", "failed"))
            .unwrap()),
        "cancelled" => {
            table
                .transition(conn, id, Expect::new(), &Transition::to(CANCELLED))
                .unwrap()
                .unwrap();
        }
        other => panic!("unknown ending {other}"),
    }
}

#[test]
fn a_terminal_job_goes_with_every_side_row_and_no_file() {
    for how in ["complete", "failed", "cancelled"] {
        let (dir, conn, table) = open();
        let (id, file) = make_job(&table, &conn, dir.path(), "r1");
        let (other, other_file) = make_job(&table, &conn, dir.path(), "r2");
        finish(&table, &conn, &id, how);
        assert_eq!(sides(&conn, &id), [1, 1, 1, 1, 1, 1], "{how}: seeded");

        let report = table.delete_terminal(&conn, &id, Expect::new()).unwrap();
        assert_eq!(report.jobs, vec![id.clone()], "{how}");
        assert_eq!(report.events, 1);
        assert_eq!(report.submissions, 1);
        assert_eq!(report.artifacts.len(), 1);
        assert_eq!(report.artifacts[0].job_id, id);
        assert_eq!(report.artifacts[0].path, file.to_string_lossy());

        assert_eq!(
            sides(&conn, &id),
            [0; 6],
            "{how}: job and every side row gone"
        );
        assert_eq!(
            sides(&conn, &other),
            [1; 6],
            "{how}: another job is untouched"
        );
        assert!(file.exists(), "{how}: the artifact file is left alone");
        assert!(other_file.exists());
        assert_eq!(
            table.get(&conn, &id).unwrap_err().code,
            "job_not_found",
            "{how}"
        );

        // Deleting it again is a not-found, not a silent success.
        assert_eq!(
            table
                .delete_terminal(&conn, &id, Expect::new())
                .unwrap_err()
                .code,
            "job_not_found"
        );
    }
}

#[test]
fn a_job_that_is_not_terminal_is_refused_and_nothing_is_written() {
    for status in ["queued", "sending", "send_failed", "held"] {
        let (dir, conn, table) = open();
        let (id, _file) = make_job(&table, &conn, dir.path(), "r1");
        let err = JobError::new("smtp", "temporary failure");
        match status {
            "queued" => {}
            "sending" => {
                table
                    .transition(&conn, &id, Expect::new(), &Transition::to("sending"))
                    .unwrap()
                    .unwrap();
            }
            "send_failed" => {
                for to in ["sending", "send_failed"] {
                    table
                        .transition(&conn, &id, Expect::new(), &Transition::to(to))
                        .unwrap()
                        .unwrap();
                }
            }
            "held" => {
                table
                    .hold_submission(&conn, &id, Expect::new(), &err, "held")
                    .unwrap()
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert_eq!(table.get(&conn, &id).unwrap().status, status);
        let before = sides(&conn, &id);
        let record = table.record(&conn, &id).unwrap();

        let refused = table
            .delete_terminal(&conn, &id, Expect::new())
            .unwrap_err();
        assert_eq!(refused.code, "illegal_transition", "{status}");
        assert!(refused.message.contains(status), "{}", refused.message);
        assert_eq!(sides(&conn, &id), before, "{status}: nothing was deleted");
        assert_eq!(
            table.record(&conn, &id).unwrap().to_value(),
            record.to_value(),
            "{status}"
        );
    }
}

#[test]
fn the_fence_and_the_revision_are_checked() {
    let (dir, conn, table) = open();
    let (id, _file) = make_job(&table, &conn, dir.path(), "r1");
    finish(&table, &conn, &id, "complete");
    let rev = table
        .revision_of(&table.record(&conn, &id).unwrap())
        .unwrap();

    let stale = table
        .delete_terminal(&conn, &id, Expect::new().fenced(99))
        .unwrap_err();
    assert_eq!(stale.code, STALE_ATTEMPT);
    let conflict = table
        .delete_terminal(&conn, &id, Expect::new().revision(rev + 1))
        .unwrap_err();
    assert_eq!(conflict.code, REVISION_CONFLICT);
    assert_eq!(sides(&conn, &id), [1; 6], "refused deletes wrote nothing");

    table
        .delete_terminal(&conn, &id, Expect::new().revision(rev))
        .unwrap();
    assert_eq!(sides(&conn, &id), [0; 6]);
}

#[test]
fn rolling_back_the_callers_transaction_restores_the_job_and_its_sides() {
    let (dir, mut conn, table) = open();
    let (id, file) = make_job(&table, &conn, dir.path(), "r1");
    finish(&table, &conn, &id, "complete");
    let record = table.record(&conn, &id).unwrap();

    {
        let tx = conn.transaction().unwrap();
        table.delete_terminal(&tx, &id, Expect::new()).unwrap();
        assert_eq!(sides(&tx, &id), [0; 6], "gone inside the transaction");
        // Dropped without commit.
    }
    assert_eq!(
        sides(&conn, &id),
        [1; 6],
        "rolled back: job and every side row are back"
    );
    assert_eq!(
        table.record(&conn, &id).unwrap().to_value(),
        record.to_value()
    );
    assert!(file.exists());

    {
        let tx = conn.transaction().unwrap();
        table.delete_terminal(&tx, &id, Expect::new()).unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(sides(&conn, &id), [0; 6], "committed: gone");
    assert!(file.exists(), "still no file effect after the commit");
}

#[test]
fn a_failing_delete_leaves_the_callers_transaction_usable_and_the_rows_intact() {
    let (dir, mut conn, table) = open();
    conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    let (id, _file) = make_job(&table, &conn, dir.path(), "r1");
    finish(&table, &conn, &id, "complete");
    // An app row that references the job: the job-row delete fails after the side rows went.
    conn.execute(
        "INSERT INTO mail_note (job_id, note) VALUES (?1, 'keep me')",
        [&id],
    )
    .unwrap();

    let tx = conn.transaction().unwrap();
    assert!(table.delete_terminal(&tx, &id, Expect::new()).is_err());
    assert_eq!(
        sides(&tx, &id),
        [1; 6],
        "the nested savepoint rolled the side rows back with the failed job delete"
    );
    // The transaction is still open and writable.
    tx.execute("DELETE FROM mail_note WHERE job_id=?1", [&id])
        .unwrap();
    table.delete_terminal(&tx, &id, Expect::new()).unwrap();
    tx.commit().unwrap();
    assert_eq!(sides(&conn, &id), [0; 6]);
}

#[test]
fn a_table_without_an_external_id_has_no_submissions_table_and_still_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let conn = Connection::open(dir.path().join("app.sqlite")).unwrap();
    let table = JobTable::new(&StoreConfig::new()).unwrap();
    table.init_schema(&conn).unwrap();
    let rec = table
        .enqueue(&conn, &new_job("r1"), &Ext::new(), &SubmitOptions::new())
        .unwrap()
        .record;
    let id = rec.job.job_id;
    let has_submissions: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='rk_job_submissions'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has_submissions, 0);
    assert!(table
        .complete(&conn, &id, Expect::new(), &json!({}))
        .unwrap());
    let report = table.delete_terminal(&conn, &id, Expect::new()).unwrap();
    assert_eq!(report.jobs, vec![id.clone()]);
    assert_eq!(report.submissions, 0);
    assert_eq!(table.get(&conn, &id).unwrap_err().code, "job_not_found");
}
