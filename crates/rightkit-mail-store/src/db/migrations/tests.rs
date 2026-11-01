use super::*;

#[test]
fn the_migration_list_is_contiguous_and_ends_at_the_schema_version() {
    for (index, step) in MIGRATIONS.iter().enumerate() {
        assert_eq!(step.version, index as i64 + 1, "{}", step.name);
    }
    assert_eq!(MIGRATIONS.last().map(|m| m.version), Some(SCHEMA_VERSION));
}

#[test]
fn v22_moves_every_queue_row_without_requeueing_a_send() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE outbox (id TEXT PRIMARY KEY, account_id TEXT NOT NULL, draft_id TEXT,
           rfc822 TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'queued',
           attempts INTEGER NOT NULL DEFAULT 0, last_error TEXT,
           created_at INTEGER NOT NULL, sent_at INTEGER, send_at INTEGER);
         CREATE INDEX idx_outbox_status ON outbox(status, created_at);
         CREATE TABLE pending_ops (id TEXT PRIMARY KEY, account_id TEXT NOT NULL,
           op_class TEXT NOT NULL, action TEXT NOT NULL, thread_id TEXT NOT NULL,
           add_labels TEXT NOT NULL, rem_labels TEXT NOT NULL,
           attempts INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL);
         INSERT INTO outbox VALUES ('q','a@x','d','raw-q','queued',0,NULL,1000,NULL,NULL);
         INSERT INTO outbox VALUES ('s','a@x',NULL,'raw-s','scheduled',0,NULL,2000,NULL,9000);
         INSERT INTO outbox VALUES ('f','a@x',NULL,'raw-f','failed',2,'boom',3000,NULL,NULL);
         INSERT INTO outbox VALUES ('d','a@x',NULL,'raw-d','sent',1,NULL,4000,4500,NULL);
         INSERT INTO outbox VALUES ('g','a@x',NULL,'raw-g','sending',1,NULL,5000,NULL,NULL);
         INSERT INTO pending_ops VALUES ('o1','a@x','label_modify','archive','t1','[\"INBOX\"]','[]',2,6000);
         INSERT INTO pending_ops VALUES ('o2','b@x','label_modify','trash','t2','junk','[\"X\"]',0,7000);",
    )
    .unwrap();
    v22_job_queue_tables(&conn).unwrap();
    // A second run finds job tables and changes nothing.
    v22_job_queue_tables(&conn).unwrap();

    let rows = crate::db::list_outbox(&conn).unwrap();
    let by_id = |id: &str| rows.iter().find(|r| r.id == id).cloned();
    assert_eq!(by_id("q").unwrap().status, "queued");
    let s = by_id("s").unwrap();
    assert_eq!((s.status.as_str(), s.send_at), ("scheduled", Some(9000)));
    let f = by_id("f").unwrap();
    assert_eq!(
        (f.status.as_str(), f.last_error.as_deref()),
        ("failed", Some("boom"))
    );
    assert_eq!(by_id("g").unwrap().status, "sending");
    assert!(by_id("d").is_none(), "a sent row stays finished");
    assert_eq!(crate::db::outbox_rfc822(&conn, "d").unwrap(), "raw-d");
    assert_eq!(crate::db::outbox_rfc822(&conn, "f").unwrap(), "raw-f");
    let attempts: i64 = conn
        .query_row(
            "SELECT attempts FROM outbox WHERE request_id = 'f'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(attempts, 2);

    let ops = crate::db::pending_ops_due(&conn, "a@x", 5, 10).unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].id, "o1");
    assert_eq!(ops[0].add_labels, vec!["INBOX".to_string()]);
    let other = crate::db::pending_ops_due(&conn, "b@x", 5, 10).unwrap();
    assert!(other[0].add_labels.is_empty());
    assert_eq!(other[0].rem_labels, vec!["X".to_string()]);
    assert!(!column_exists(&conn, "outbox_pre_v22", "id").unwrap());
}
