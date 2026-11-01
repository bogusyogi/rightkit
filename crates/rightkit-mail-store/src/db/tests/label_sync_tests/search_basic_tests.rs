use super::*;

#[test]
fn search_threads_matches_via_fts() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    apply_messages(&conn, "a@x.com", &[msg("t1", &["INBOX"], false, 10)]).unwrap();
    // msg() subject is "subj". This also proves the query PREPARES — the old
    // aliased `f MATCH ?1` raised "no such column: f" and broke all search.
    let hits = search_threads_scoped(&conn, "subj", None, None, None).unwrap();
    assert_eq!(hits.len(), 1, "FTS finds the seeded thread");
    assert_eq!(hits[0].thread_id, "t1");
    assert_eq!(hits[0].match_count, 1);
    assert_eq!(hits[0].matching_message_id.as_deref(), Some("t1-10"));
    assert!(
        hits[0]
            .matching_message_excerpt
            .as_deref()
            .unwrap_or_default()
            .contains("subj"),
        "search rows include the matched message excerpt"
    );
    assert!(
        search_threads_scoped(&conn, "zzzznomatch", None, None, None)
            .unwrap()
            .is_empty(),
        "non-match returns nothing"
    );
}

#[test]
fn apply_search_messages_indexes_fts() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    let mut m = msg("t-search", &["INBOX"], false, 10);
    m.subject = "Needle receipt".into();
    apply_search_messages(&conn, "a@x.com", &[m]).unwrap();

    let hits = search_threads_scoped(&conn, "needle", None, None, None).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "server-search upserts are locally searchable"
    );
    assert_eq!(hits[0].thread_id, "t-search");
}

// L1 regression: the hot paths (load_older, get_thread, backfill, save_draft)
// were converted from apply_messages (full O(mailbox) rebuild_rollups) to
// apply_search_messages (incremental — only the touched threads/messages are
// rebuilt). This proves an incremental batch that only touches thread A never
// disturbs an untouched thread B's rollup row or FTS row.
#[test]
fn incremental_apply_leaves_other_threads_rollups_and_fts_untouched() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    let thread_a = msg("thread-a", &["INBOX"], false, 10);
    let thread_b = msg("thread-b", &["INBOX"], true, 20);
    apply_messages(&conn, "a@x.com", &[thread_a.clone(), thread_b.clone()]).unwrap();

    let thread_b_row_before: (String, String, i64, i64) = conn
        .query_row(
            "SELECT subject, snippet, unread, last_message_at FROM threads
             WHERE account_id = 'a@x.com' AND id = 'thread-b'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    let thread_b_labels_before: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT label FROM thread_labels
                 WHERE account_id = 'a@x.com' AND thread_id = 'thread-b' ORDER BY label",
            )
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let thread_b_fts_before: (i64, String) = conn
        .query_row(
            "SELECT rowid, message_id FROM fts_messages
             WHERE account_id = 'a@x.com' AND message_id = ?1",
            [&thread_b.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();

    // Incremental batch that touches ONLY thread A (mirrors load_older/get_thread
    // fetching more of thread A's history — thread B is never in this slice).
    let mut thread_a_followup = msg("thread-a", &["INBOX"], true, 30);
    thread_a_followup.subject = "follow-up".into();
    apply_search_messages(&conn, "a@x.com", &[thread_a_followup]).unwrap();

    let thread_b_row_after: (String, String, i64, i64) = conn
        .query_row(
            "SELECT subject, snippet, unread, last_message_at FROM threads
             WHERE account_id = 'a@x.com' AND id = 'thread-b'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        thread_b_row_before, thread_b_row_after,
        "untouched thread B's rollup row must survive byte-identical"
    );

    let thread_b_labels_after: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT label FROM thread_labels
                 WHERE account_id = 'a@x.com' AND thread_id = 'thread-b' ORDER BY label",
            )
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        thread_b_labels_before, thread_b_labels_after,
        "untouched thread B's thread_labels must survive"
    );

    let thread_b_fts_after: (i64, String) = conn
        .query_row(
            "SELECT rowid, message_id FROM fts_messages
             WHERE account_id = 'a@x.com' AND message_id = ?1",
            [&thread_b.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        thread_b_fts_before, thread_b_fts_after,
        "untouched thread B's fts row must survive byte-identical"
    );

    // Sanity: thread A DID pick up the incremental change.
    let thread_a_count: i64 = conn
        .query_row(
            "SELECT message_count FROM threads WHERE account_id = 'a@x.com' AND id = 'thread-a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        thread_a_count, 2,
        "thread A rollup reflects the new message"
    );
}

#[test]
fn search_threads_scoped_respects_account_and_label() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    upsert_account(&conn, "b@x.com").unwrap();
    apply_messages(
        &conn,
        "a@x.com",
        &[
            msg("a-inbox", &["INBOX"], false, 30),
            msg("a-sent", &["SENT"], false, 20),
        ],
    )
    .unwrap();
    apply_messages(&conn, "b@x.com", &[msg("b-inbox", &["INBOX"], false, 10)]).unwrap();

    let all_inboxes = search_threads_scoped(&conn, "subj", None, Some("INBOX"), None).unwrap();
    let all_ids: Vec<&str> = all_inboxes.iter().map(|t| t.thread_id.as_str()).collect();
    assert_eq!(all_ids, vec!["a-inbox", "b-inbox"]);

    let account_inbox =
        search_threads_scoped(&conn, "subj", Some("a@x.com"), Some("INBOX"), None).unwrap();
    let account_ids: Vec<&str> = account_inbox.iter().map(|t| t.thread_id.as_str()).collect();
    assert_eq!(account_ids, vec!["a-inbox"]);

    let account_all = search_threads_scoped(&conn, "subj", Some("a@x.com"), None, None).unwrap();
    let account_all_ids: Vec<&str> = account_all.iter().map(|t| t.thread_id.as_str()).collect();
    assert_eq!(account_all_ids, vec!["a-inbox", "a-sent"]);
}

// Regression: free-text with FTS5 special chars must not raise a syntax
// error (was "fts5: syntax error near /"). Terms are quoted phrases now.
#[test]
fn search_tolerates_fts_special_chars() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    apply_messages(&conn, "a@x.com", &[msg("t1", &["INBOX"], false, 10)]).unwrap();
    for q in [
        "appeal/review",
        "a:b",
        "c^d",
        "(foo)",
        "- minus",
        "/",
        "***",
    ] {
        assert!(
            search_threads_scoped(&conn, q, None, None, None).is_ok(),
            "query {q:?} must not crash FTS5"
        );
    }
}
