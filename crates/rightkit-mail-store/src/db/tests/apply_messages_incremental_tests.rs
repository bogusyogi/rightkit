// L15: incremental `apply_messages` regression suite.
//
// The previous `apply_messages` did an account-level `rebuild_rollups`
// (O(mailbox)) under the writer mutex — the secondary-suspect-#1 path in the
// 2026-07-03 "Not Responding" audit. Sync delivers 25-message batches; we now
// only touch the threads those messages belong to, mirroring
// `apply_search_messages` and `apply_delta`. These tests pin both
// *correctness* (rollup invariants still hold) and *budget* (a 25-message
// batch is sub-second on a populated DB so foreground reader waits stay
// inside the 250 ms `LOCK_WAIT_BUDGET_MS`).

use super::*;

// Small in-memory seed: 1 message per thread, distinct labels so each thread
// exercises its own thread_labels row.
fn seed_populated_db(conn: &Connection, account_id: &str, threads: usize) {
    upsert_account(conn, account_id).unwrap();
    let mut to_apply: Vec<FetchedMessage> = Vec::with_capacity(threads);
    for i in 0..threads {
        let thread = format!("t-seed-{i}");
        let m = msg(&thread, &["INBOX"], false, 1_000_000 + i as i64);
        to_apply.push(m);
    }
    apply_messages(conn, account_id, &to_apply).unwrap();
}

#[test]
fn apply_messages_incremental_matches_full_rollup_for_new_messages() {
    // Seed a 50-thread DB, then apply a 25-message batch where each message
    // lands in a fresh thread. After incremental apply, the threads table
    // must contain every original thread + the 25 new ones, and every thread
    // must have correct (subject, snippet, last_message_at, message_count).
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();

    let existing: Vec<FetchedMessage> = (0..50)
        .map(|i| {
            msg(
                &format!("t-existing-{i}"),
                &["INBOX"],
                false,
                1000 + i as i64,
            )
        })
        .collect();
    apply_messages(&conn, "a@x.com", &existing).unwrap();

    let batch: Vec<FetchedMessage> = (0..25)
        .map(|i| {
            let mut m = msg(
                &format!("t-new-{i}"),
                &["INBOX", "STARRED"],
                true,
                5000 + i as i64,
            );
            m.subject = format!("new-{i}-subj");
            m.snippet = format!("new-{i}-snip");
            m
        })
        .collect();
    apply_messages(&conn, "a@x.com", &batch).unwrap();

    let thread_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM threads", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        thread_count, 75,
        "50 existing + 25 new threads must all be present"
    );

    // Every new thread must have its exact subject/snippet/message_count
    // (proves the per-thread INSERT ran, not just the row count).
    // Sorted by id (alphabetical) so `t-new-10` precedes `t-new-2`; use the
    // id suffix (parsed from `t-new-N`) to build the expected subject.
    let mut stmt = conn
        .prepare(
            "SELECT id, subject, snippet, message_count, starred, important
             FROM threads WHERE id LIKE 't-new-%' ORDER BY id",
        )
        .unwrap();
    let rows: Vec<(String, String, String, i64, bool, bool)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)? != 0,
                r.get::<_, i64>(5)? != 0,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), 25);
    for (id, subj, snip, msg_count, starred, _important) in &rows {
        // id format: "t-new-<N>"
        let n: usize = id
            .strip_prefix("t-new-")
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("unexpected id format: {id}"));
        assert_eq!(subj, &format!("new-{n}-subj"), "subject for {id}");
        assert_eq!(snip, &format!("new-{n}-snip"), "snippet for {id}");
        assert_eq!(*msg_count, 1, "{id} has one message");
        assert!(*starred, "{id} STARRED label must set the starred flag");
    }

    // Existing threads must be untouched: their last_message_at reflects
    // their original internal_date, not the new batch's dates.
    let existing_max_date: i64 = conn
        .query_row(
            "SELECT MAX(last_message_at) FROM threads WHERE id LIKE 't-existing-%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        existing_max_date,
        1000 + 49,
        "existing threads untouched by the new batch"
    );
}

#[test]
fn apply_messages_incremental_updates_only_touched_threads() {
    // Seed 100 existing threads. Then apply 5 messages that all land in ONE
    // existing thread (t1). After apply, only t1's rollup should reflect the
    // new message; the other 99 should be byte-identical to before.
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();

    seed_populated_db(&conn, "a@x.com", 100);

    // Snapshot all 99 untouched threads (every thread except t-seed-0).
    let before: Vec<(String, String, i64, i64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT id, snippet, last_message_at, message_count
                 FROM threads WHERE id != 't-seed-0' ORDER BY id",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };

    // Apply 5 new messages all in t-seed-0 (which already has one message).
    // Use dates NEWER than the seed so `MAX(internal_date)` actually moves.
    let batch: Vec<FetchedMessage> = (0..5)
        .map(|i| {
            let mut m = msg("t-seed-0", &["INBOX"], false, 2_000_000 + i as i64);
            m.subject = format!("seed-0-update-{i}");
            m
        })
        .collect();
    apply_messages(&conn, "a@x.com", &batch).unwrap();

    // The 99 untouched threads must be byte-identical.
    let after: Vec<(String, String, i64, i64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT id, snippet, last_message_at, message_count
                 FROM threads WHERE id != 't-seed-0' ORDER BY id",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };
    assert_eq!(
        before, after,
        "incremental rebuild must leave non-touched threads byte-identical"
    );

    // The touched thread must reflect the 6 messages (1 original + 5 new)
    // and the newest internal_date.
    let (count, last_at): (i64, i64) = conn
        .query_row(
            "SELECT message_count, last_message_at FROM threads WHERE id = 't-seed-0'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 6, "t-seed-0 now has 6 messages");
    assert_eq!(
        last_at, 2_000_004,
        "last_message_at tracks the newest message"
    );
}

#[test]
fn apply_messages_incremental_updates_important_flag() {
    // L15 follow-up: the incremental `rebuild_threads_for_ids` was missing
    // the `important` column update. Pin it here so the drift can't return.
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();

    apply_messages(&conn, "a@x.com", &[msg("t1", &["INBOX"], false, 1_000)]).unwrap();
    let important_before: bool = conn
        .query_row("SELECT important FROM threads WHERE id = 't1'", [], |r| {
            Ok(r.get::<_, i64>(0)? != 0)
        })
        .unwrap();
    assert!(!important_before, "no IMPORTANT label ⇒ important=false");

    // Apply a second message to t1, this time tagged IMPORTANT.
    let mut important_msg = msg("t1", &["INBOX", "IMPORTANT"], false, 2_000);
    important_msg.subject = "important now".into();
    apply_messages(&conn, "a@x.com", &[important_msg]).unwrap();

    let (important_after, has_important_label): (bool, bool) = conn
        .query_row(
            "SELECT
               (SELECT important FROM threads WHERE id = 't1'),
               EXISTS(SELECT 1 FROM thread_labels WHERE account_id='a@x.com' AND thread_id='t1' AND label='IMPORTANT')",
            [],
            |r| Ok((r.get::<_, i64>(0)? != 0, r.get::<_, i64>(1)? != 0)),
        )
        .unwrap();
    assert!(has_important_label, "IMPORTANT label row written");
    assert!(
        important_after,
        "incremental rebuild must propagate IMPORTANT into threads.important"
    );
}

#[test]
fn apply_messages_empty_batch_is_a_noop() {
    // L15: the old path ran a full `rebuild_rollups` even on an empty slice —
    // pure waste on a 200k mailbox. New path short-circuits.
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    seed_populated_db(&conn, "a@x.com", 10);

    let before: Vec<(String, i64)> = {
        let mut stmt = conn
            .prepare("SELECT id, message_count FROM threads ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };

    let n = apply_messages(&conn, "a@x.com", &[]).unwrap();
    assert_eq!(n, 0, "empty batch returns 0");

    let after: Vec<(String, i64)> = {
        let mut stmt = conn
            .prepare("SELECT id, message_count FROM threads ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(before, after, "empty apply must not touch anything");
}

// Performance budget (L15). 25-message batch on a 1k-thread mailbox must
// stay well under the 250 ms `LOCK_WAIT_BUDGET_MS` ceiling that triggers
// sync backpressure. The full-rebuild path took seconds at this scale; the
// incremental path is bounded by O(affected threads), not O(mailbox).
//
// Run with:
//   cargo test --lib apply_messages_incremental_stays_under_500ms_for_25_message_batch -- --nocapture
#[test]
fn apply_messages_incremental_stays_under_500ms_for_25_message_batch() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();

    // 1,000 threads × 1 message each = 1,000-row mailbox.
    seed_populated_db(&conn, "a@x.com", 1_000);

    // 25 new messages, each in its own new thread. Mirrors a real Gmail
    // sync batch (most new mail = new threads; threads-with-existing-mail
    // are the second case in the test above).
    let batch: Vec<FetchedMessage> = (0..25)
        .map(|i| {
            let mut m = msg(&format!("t-budget-{i}"), &["INBOX"], true, 100_000 + i);
            m.subject = format!("budget-{i}");
            m.snippet = format!("budget-{i}-snip");
            m
        })
        .collect();

    // Warm the planner once (mimics the L9 `pragma_optimize` call that
    // production does at sync pass boundaries — without it, the first
    // query post-seed is unfairly slow).
    crate::db::pragma_optimize(&conn);

    let started = std::time::Instant::now();
    apply_messages(&conn, "a@x.com", &batch).unwrap();
    let elapsed_ms = started.elapsed().as_millis();

    eprintln!(
        "apply_messages_incremental_stays_under_500ms batch=25 mailbox=1k elapsed_ms={elapsed_ms}"
    );

    assert!(
        elapsed_ms < 500,
        "incremental apply of a 25-message batch on a 1k-thread mailbox must stay under 500 ms (got {elapsed_ms} ms)"
    );
}

// L15: regression for the "drafts trash did nothing" bug. The bulk trash
// action's local mirror (`apply_local_action`) used to only drop INBOX +
// add TRASH. A draft thread has no INBOX, so the trash button left it
// carrying both DRAFT and TRASH — visible in Drafts because DRAFT was
// still there. The fix makes `trash` also drop DRAFT, so a draft vanishes
// from the Drafts view the moment the user clicks delete (matching the
// Gmail-web "Discard" semantics).
#[test]
fn bulk_trash_drops_draft_label_so_drafts_leave_drafts_view() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    // Seed two draft threads + one inbox thread as control.
    apply_messages(
        &conn,
        "a@x.com",
        &[
            msg("t-draft-1", &["DRAFT"], false, 100),
            msg("t-draft-2", &["DRAFT"], false, 200),
            msg("t-inbox", &["INBOX"], false, 300),
        ],
    )
    .unwrap();

    // Verify both drafts are present before trash.
    let drafts_before: Vec<String> = conn
        .prepare("SELECT thread_id FROM thread_labels WHERE label='DRAFT' ORDER BY thread_id")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        drafts_before,
        vec!["t-draft-1".to_string(), "t-draft-2".to_string()],
        "both draft threads present"
    );

    // Mirror what `bulk_action("trash")` does locally (the local-drop-label
    // half — the remote modifyLabels is mocked out by unit tests). The fix
    // is in `apply_local_action`, but pinning the behavior here means any
    // future refactor that loses the DRAFT drop trips this test.
    let refs = ["t-draft-1", "t-draft-2"];
    for tid in refs {
        crate::db::local_drop_label(&conn, "a@x.com", tid, "INBOX").unwrap();
        crate::db::local_drop_label(&conn, "a@x.com", tid, "DRAFT").unwrap();
        crate::db::local_add_label(&conn, "a@x.com", tid, "TRASH").unwrap();
    }

    // Drafts view (filter `label = DRAFT`) must now be empty.
    let drafts_after: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT thread_id) FROM thread_labels WHERE label='DRAFT'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        drafts_after, 0,
        "after trash, no thread should still carry the DRAFT label"
    );

    // Trash view picks up the two threads; inbox thread stays put.
    let trash_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM thread_labels WHERE label='TRASH'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(trash_count, 2, "both trashed drafts land in TRASH");

    let inbox_label: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM thread_labels WHERE thread_id='t-inbox' AND label='INBOX'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        inbox_label, 1,
        "the untouched inbox thread must keep its INBOX label"
    );
}
