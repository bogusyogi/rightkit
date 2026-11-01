use super::*;

// Schema-v9 invariant: every fts_messages row is keyed rowid = messages.rowid,
// which is what makes per-message FTS maintenance a rowid lookup instead of a
// full scan of every indexed body (the 2026-07-03 sync-hang bug). These tests
// pin the invariant across the three writers (incremental apply, delta delete,
// migration rebuild).

type RowidPairs = Vec<(i64, String)>;

fn fts_and_message_rowids(conn: &Connection) -> (RowidPairs, RowidPairs) {
    let mut stmt = conn
        .prepare("SELECT rowid, message_id FROM fts_messages ORDER BY message_id")
        .unwrap();
    let fts: RowidPairs = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut stmt = conn
        .prepare("SELECT rowid, id FROM messages ORDER BY id")
        .unwrap();
    let msgs: RowidPairs = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    (fts, msgs)
}

#[test]
fn apply_keys_fts_rows_by_message_rowid() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    let msgs = vec![
        msg("t1", &["INBOX"], true, 20),
        msg("t2", &["SENT"], false, 10),
    ];
    apply_messages(&conn, "a@x.com", &msgs).unwrap();

    let (fts, messages) = fts_and_message_rowids(&conn);
    assert_eq!(fts.len(), 2);
    assert_eq!(fts, messages, "fts rowids must mirror messages rowids");

    // Re-applying (upsert path) must not duplicate or re-key rows.
    apply_messages(&conn, "a@x.com", &msgs).unwrap();
    let (fts2, messages2) = fts_and_message_rowids(&conn);
    assert_eq!(fts2, fts, "re-apply keeps the same fts rowids");
    assert_eq!(messages2, messages, "upsert preserves messages rowids");
}

#[test]
fn delta_delete_removes_exactly_the_deleted_fts_row() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    let m1 = msg("t1", &["INBOX"], true, 20);
    let m2 = msg("t2", &["INBOX"], false, 10);
    apply_messages(&conn, "a@x.com", &[m1.clone(), m2.clone()]).unwrap();

    apply_delta(&conn, "a@x.com", &[], std::slice::from_ref(&m1.id)).unwrap();

    let (fts, messages) = fts_and_message_rowids(&conn);
    assert_eq!(messages.len(), 1);
    assert_eq!(
        fts, messages,
        "surviving fts row still keyed to its message"
    );
    assert_eq!(
        fts[0].1, m2.id,
        "only the non-deleted message stays indexed"
    );
}

#[test]
fn migration_v9_rekeys_legacy_fts_rows() {
    // Build a pre-v9 DB: schema v1, one message, and an fts row inserted the
    // legacy way (auto rowid, unrelated to messages.rowid).
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    conn.execute_batch(SCHEMA_V1).unwrap();
    conn.execute_batch("PRAGMA user_version = 1;").unwrap();
    conn.execute(
        "INSERT INTO accounts(id, display_name, color, created_at)
           VALUES('a@x.com','a','#fff',1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages(account_id,id,thread_id,subject,snippet,internal_date,label_ids,unread,has_attachments)
           VALUES('a@x.com','m1','t1','hello world','snip',1,'[\"INBOX\"]',1,0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO fts_messages(rowid, message_id, account_id, subject, from_text, body)
           VALUES(999,'m1','a@x.com','hello world','', 'snip')",
        [],
    )
    .unwrap();

    migrate(&conn).unwrap();

    let (fts, messages) = fts_and_message_rowids(&conn);
    assert_eq!(fts.len(), 1, "rebuild keeps exactly one row per message");
    assert_eq!(fts, messages, "legacy fts row re-keyed to messages.rowid");
    let hits: i64 = conn
        .query_row(
            "SELECT count(*) FROM fts_messages WHERE fts_messages MATCH '\"hello\"'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hits, 1, "content still searchable after the rebuild");
}
