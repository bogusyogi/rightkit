// Chunked account rollup rebuild (writer-hold bounding at download-all
// completion). Pins: the chunked result equals the single-transaction
// `rebuild_all_rollups`, a write between chunks leaves both processed and
// unprocessed threads correct, and chunk size 1 / oversize both work.

use super::*;

const A: &str = "a@x.com";
const B: &str = "b@x.com";

fn tagged(thread: &str, labels: &[&str], unread: bool, date: i64, subject: &str) -> FetchedMessage {
    let mut m = msg(thread, labels, unread, date);
    m.subject = subject.to_string();
    m.snippet = format!("snip-{date}");
    m
}

/// Out-of-order, multi-batch, multi-account mailbox.
fn seed(conn: &Connection) {
    upsert_account(conn, A).unwrap();
    upsert_account(conn, B).unwrap();
    // Newest batch first, older messages of the same threads arrive later.
    for (base, labels) in [(5000i64, &["INBOX", "STARRED"][..]), (1000, &["INBOX"][..])] {
        let batch: Vec<FetchedMessage> = (0..7)
            .map(|i| {
                let imp: &[&str] = if i % 3 == 0 { &["IMPORTANT"] } else { &[] };
                let mut l: Vec<&str> = labels.to_vec();
                l.extend_from_slice(imp);
                tagged(
                    &format!("t{i}"),
                    &l,
                    i % 2 == 0,
                    base + i as i64,
                    &format!("alpha{i} common"),
                )
            })
            .collect();
        apply_messages(conn, A, &batch).unwrap();
    }
    let other: Vec<FetchedMessage> = (0..4)
        .map(|i| {
            tagged(
                &format!("t{i}"),
                &["SENT"],
                false,
                3000 + i,
                &format!("beta{i}"),
            )
        })
        .collect();
    apply_messages(conn, B, &other).unwrap();
}

/// Wipe derived state and plant an orphan so the rebuild has real work.
fn corrupt(conn: &Connection) {
    conn.execute_batch(
        "DELETE FROM threads; DELETE FROM thread_labels; DELETE FROM message_search;",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO threads(account_id, id, subject, snippet, unread, starred, last_message_at, message_count)
         VALUES(?1, 'orphan', 's', 's', 0, 0, 1, 1)",
        [A],
    )
    .unwrap();
}

fn snapshot(conn: &Connection) -> String {
    let mut out = String::new();
    let mut st = conn
        .prepare(
            "SELECT account_id, id, subject, snippet, unread, starred, important,
                    last_message_at, message_count FROM threads ORDER BY account_id, id",
        )
        .unwrap();
    let rows = st
        .query_map([], |r| {
            Ok(format!(
                "{:?}",
                (
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, i64>(8)?
                )
            ))
        })
        .unwrap();
    for r in rows {
        out.push_str(&r.unwrap());
        out.push('\n');
    }
    let mut st = conn
        .prepare("SELECT account_id, thread_id, label FROM thread_labels ORDER BY 1, 2, 3")
        .unwrap();
    let rows = st
        .query_map([], |r| {
            Ok(format!(
                "{}/{}/{}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?
            ))
        })
        .unwrap();
    for r in rows {
        out.push_str(&r.unwrap());
        out.push('\n');
    }
    for term in ["alpha0", "alpha3", "alpha6", "beta2", "common"] {
        let mut ids: Vec<String> = search_threads_scoped(conn, term, None, None, None)
            .unwrap()
            .into_iter()
            .map(|t| format!("{}/{}", t.account_id, t.thread_id))
            .collect();
        ids.sort();
        out.push_str(&format!("{term}: {ids:?}\n"));
    }
    out
}

fn reference(extra: impl Fn(&Connection)) -> String {
    let conn = test_conn();
    seed(&conn);
    corrupt(&conn);
    extra(&conn);
    rebuild_all_rollups(&conn, A).unwrap();
    rebuild_all_rollups(&conn, B).unwrap();
    snapshot(&conn)
}

fn new_messages(conn: &Connection) {
    // One lands in an early thread, one in a late thread, one is brand new.
    let batch = vec![
        tagged("t0", &["INBOX", "STARRED"], true, 9000, "alpha0 common"),
        tagged("t5", &["INBOX"], true, 9001, "alpha5 common"),
        tagged("t9", &["INBOX", "IMPORTANT"], true, 9002, "alpha9 common"),
    ];
    apply_messages(conn, A, &batch).unwrap();
}

#[test]
fn chunked_rebuild_matches_single_transaction_for_any_chunk_size() {
    let want = reference(|_| {});
    assert!(want.contains("alpha3"), "search hits must be exercised");
    assert!(!want.contains("orphan"), "reference drops orphan threads");
    for size in [1usize, 2, 3, 100, 100_000] {
        let conn = test_conn();
        seed(&conn);
        corrupt(&conn);
        rebuild_all_rollups_chunked(&conn, A, size).unwrap();
        rebuild_all_rollups_chunked(&conn, B, size).unwrap();
        assert_eq!(snapshot(&conn), want, "chunk size {size}");
    }
}

#[test]
fn chunk_cursor_advances_and_terminates() {
    let conn = test_conn();
    seed(&conn);
    let first = rebuild_rollups_chunk(&conn, A, None, 3).unwrap();
    assert_eq!(first.as_deref(), Some("t2"));
    let second = rebuild_rollups_chunk(&conn, A, first.as_deref(), 3).unwrap();
    assert_eq!(second.as_deref(), Some("t5"));
    assert_eq!(
        rebuild_rollups_chunk(&conn, A, second.as_deref(), 3).unwrap(),
        None
    );
    // Empty account: finishes immediately.
    assert_eq!(
        rebuild_rollups_chunk(&conn, "none@x.com", None, 3).unwrap(),
        None
    );
}

#[test]
fn write_between_chunks_leaves_processed_and_unprocessed_threads_correct() {
    let want = reference(new_messages);
    let conn = test_conn();
    seed(&conn);
    corrupt(&conn);
    // Process t0..t2, then a sync lands before the rest are reached.
    let cursor = rebuild_rollups_chunk(&conn, A, None, 3).unwrap();
    assert_eq!(cursor.as_deref(), Some("t2"));
    new_messages(&conn);
    let mut cursor = cursor;
    while cursor.is_some() {
        cursor = rebuild_rollups_chunk(&conn, A, cursor.as_deref(), 3).unwrap();
    }
    rebuild_all_rollups_chunked(&conn, B, 3).unwrap();
    assert_eq!(snapshot(&conn), want);
}
