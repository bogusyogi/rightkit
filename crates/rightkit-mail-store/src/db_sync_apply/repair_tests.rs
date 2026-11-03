// Download-all completion repair (`repair_rollups_chunk`): a consistent mailbox
// is left byte-for-byte alone (no full-text writes, vectors and semantic
// cursor intact), broken rollups and orphans are repaired, and a write between
// chunks stays correct.

use super::*;
use crate::db::semantic::embedder::STORED_DIM;
use crate::db::semantic::storage;
use rightkit_google_apis::gmail::FetchedMessage;

const A: &str = "a@x.com";

fn message(thread: &str, id: &str, unread: bool, date: i64) -> FetchedMessage {
    FetchedMessage {
        id: id.to_string(),
        thread_id: thread.to_string(),
        from_name: "S".into(),
        from_addr: "s@x.com".into(),
        to_addrs: "me@x.com".into(),
        cc_addrs: String::new(),
        reply_to: None,
        list_unsubscribe: None,
        auth_results: None,
        subject: format!("subject {thread}"),
        snippet: format!("snippet {id}"),
        internal_date: date,
        unread,
        label_ids: vec!["INBOX".to_string()],
        body_plain: Some(format!("body of {id}")),
        body_html: None,
        has_attachments: false,
        attachments: Vec::new(),
        size_estimate: None,
        rfc822_message_id: None,
    }
}

/// Six threads of two messages each, applied through the normal apply path.
fn seed(conn: &Connection) {
    upsert_account(conn, A).unwrap();
    let batch: Vec<FetchedMessage> = (0..6)
        .flat_map(|i| {
            let t = format!("t{i}");
            [
                message(&t, &format!("{t}-old"), false, 1000 + i),
                message(&t, &format!("{t}-new"), i % 2 == 0, 2000 + i),
            ]
        })
        .collect();
    apply_messages(conn, A, &batch).unwrap();
}

/// Give every message a vector and a finished semantic index.
fn embed_everything(conn: &Connection) {
    let mut v = [0f32; STORED_DIM];
    v[0] = 1.0;
    let rowids: Vec<i64> = conn
        .prepare("SELECT rowid FROM messages ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    for rowid in &rowids {
        storage::upsert_vector(conn, *rowid, &v).unwrap();
    }
    let mut st = storage::get_index_state(conn, A).unwrap();
    st.embed_model_id = "fake".to_string();
    st.cursor_rowid = *rowids.last().unwrap();
    st.indexed_count = rowids.len() as i64;
    st.done = true;
    storage::upsert_index_state(conn, &st).unwrap();
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

type Rollup = (String, i64, i64, i64);

fn stored(conn: &Connection) -> Vec<Rollup> {
    conn.prepare(
        "SELECT id, message_count, last_message_at, unread FROM threads
         WHERE account_id = ?1 ORDER BY id",
    )
    .unwrap()
    .query_map([A], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

fn expected(conn: &Connection) -> Vec<Rollup> {
    conn.prepare(
        "SELECT thread_id, COUNT(*), MAX(internal_date), MAX(unread) FROM messages
         WHERE account_id = ?1 GROUP BY thread_id ORDER BY thread_id",
    )
    .unwrap()
    .query_map([A], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

#[test]
fn repair_on_a_consistent_mailbox_changes_nothing() {
    let conn = test_conn();
    seed(&conn);
    embed_everything(&conn);
    let rollups = stored(&conn);
    let search = count(&conn, "message_search");
    let vectors = count(&conn, "message_vec");
    let state = storage::get_index_state(&conn, A).unwrap();
    assert!(state.done && vectors == 12 && search > 0, "test premise");

    for size in [1usize, 5, REPAIR_CHUNK_THREADS] {
        let before = conn.total_changes();
        repair_all_rollups_chunked(&conn, A, size).unwrap();
        assert_eq!(conn.total_changes(), before, "no row written, chunk {size}");
    }

    assert_eq!(stored(&conn), rollups);
    assert_eq!(count(&conn, "message_search"), search);
    assert_eq!(count(&conn, "message_vec"), vectors);
    assert_eq!(storage::get_index_state(&conn, A).unwrap(), state);
}

#[test]
fn repair_fixes_a_wrong_rollup_without_touching_vectors() {
    let conn = test_conn();
    seed(&conn);
    embed_everything(&conn);
    let state = storage::get_index_state(&conn, A).unwrap();
    let want = expected(&conn);
    conn.execute_batch(
        "UPDATE threads SET message_count = 99 WHERE id = 't1';
         UPDATE threads SET unread = 1 - unread WHERE id = 't3';
         UPDATE threads SET last_message_at = 7 WHERE id = 't4';
         DELETE FROM threads WHERE id = 't5';",
    )
    .unwrap();
    assert_ne!(stored(&conn), want, "test premise");

    repair_all_rollups_chunked(&conn, A, 2).unwrap();

    assert_eq!(stored(&conn), want);
    assert_eq!(count(&conn, "message_vec"), 12);
    assert_eq!(storage::get_index_state(&conn, A).unwrap(), state);
}

#[test]
fn repair_drops_orphan_threads_and_labels() {
    let conn = test_conn();
    seed(&conn);
    conn.execute(
        "INSERT INTO threads(account_id, id, subject, snippet, unread, starred, last_message_at, message_count)
         VALUES(?1, 'orphan', 's', 's', 0, 0, 1, 1)",
        [A],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO thread_labels(account_id, thread_id, label) VALUES(?1, 'orphan', 'INBOX')",
        [A],
    )
    .unwrap();

    repair_all_rollups_chunked(&conn, A, 2).unwrap();

    for (table, col) in [("threads", "id"), ("thread_labels", "thread_id")] {
        let n: i64 = conn
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE {col} = 'orphan'"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "{table} orphan removed");
    }
    assert_eq!(stored(&conn), expected(&conn));
}

#[test]
fn write_between_repair_chunks_stays_correct() {
    let conn = test_conn();
    seed(&conn);
    conn.execute_batch("UPDATE threads SET message_count = 50 WHERE id IN ('t0', 't4')")
        .unwrap();

    // Check t0..t1, then a sync lands in a processed thread (t0), an
    // unprocessed broken one (t4) and a brand-new one (t9).
    let cursor = repair_rollups_chunk(&conn, A, None, 2).unwrap();
    assert_eq!(cursor.as_deref(), Some("t1"));
    apply_messages(
        &conn,
        A,
        &[
            message("t0", "t0-extra", true, 9000),
            message("t4", "t4-extra", true, 9001),
            message("t9", "t9-only", true, 9002),
        ],
    )
    .unwrap();
    let mut cursor = cursor;
    while cursor.is_some() {
        cursor = repair_rollups_chunk(&conn, A, cursor.as_deref(), 2).unwrap();
    }

    assert_eq!(stored(&conn), expected(&conn));
}
