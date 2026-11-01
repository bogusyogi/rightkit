use super::*;
use crate::db::fts_index::{self, RebuildStep};

// Schema v15: the full-text index is an external-content FTS5 table over
// plain text extracted from the body (message_search). These tests pin the
// extraction, the per-message maintenance and the resumable rebuild.

fn add_message(conn: &Connection, id: &str, subject: &str, mime: &str, body: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO accounts(id, created_at) VALUES('a1', 0)",
        [],
    )
    .ok();
    conn.execute(
        "INSERT INTO messages(account_id, id, thread_id, subject, snippet, internal_date)
         VALUES('a1', ?1, 't-'||?1, ?2, 'snip', 0)",
        params![id, subject],
    )
    .unwrap();
    set_body(conn, id, mime, body);
}

fn set_body(conn: &Connection, id: &str, mime: &str, body: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO message_bodies(account_id, message_id, mime_type, body)
         VALUES('a1', ?1, ?2, ?3)",
        params![id, mime, body],
    )
    .unwrap();
}

fn hits(conn: &Connection, q: &str) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM fts_messages WHERE fts_messages MATCH ?1",
        [q],
        |r| r.get(0),
    )
    .unwrap()
}

fn integrity_ok(conn: &Connection) {
    conn.execute(
        "INSERT INTO fts_messages(fts_messages, rank) VALUES('integrity-check', 1)",
        [],
    )
    .expect("fts integrity-check");
}

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn html_text_is_searchable_and_markup_is_not() {
    let conn = test_conn();
    add_message(
        &conn,
        "m1",
        "report",
        "text/html",
        "<div class=\"wrapperbox\" style=\"color:red\"><b>Quarterly</b> zeppelin report</div>",
    );
    fts_index::index_messages(&conn, "a1", &ids(&["m1"])).unwrap();

    assert_eq!(hits(&conn, "zeppelin"), 1);
    assert_eq!(hits(&conn, "zep*"), 1, "prefix query");
    for markup in ["div", "wrapperbox", "style", "class", "color"] {
        assert_eq!(
            hits(&conn, markup),
            0,
            "markup term {markup} must not match"
        );
    }
    let snip: String = conn
        .query_row(
            "SELECT snippet(fts_messages, -1, '[', ']', '...', 8)
             FROM fts_messages WHERE fts_messages MATCH 'zeppelin'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(snip.contains("[zeppelin]"), "snippet highlights: {snip}");
    let stored: String = conn
        .query_row(
            "SELECT body FROM message_search WHERE message_id='m1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!stored.contains('<'), "stored body is plain text: {stored}");
    integrity_ok(&conn);
}

#[test]
fn update_and_delete_keep_the_index_consistent() {
    let conn = test_conn();
    add_message(&conn, "m1", "s1", "text/html", "<p>alpha marker</p>");
    add_message(&conn, "m2", "s2", "text/plain", "beta marker");
    fts_index::index_messages(&conn, "a1", &ids(&["m1", "m2"])).unwrap();
    assert_eq!(hits(&conn, "marker"), 2);

    set_body(&conn, "m1", "text/html", "<p>gamma marker</p>");
    fts_index::index_messages(&conn, "a1", &ids(&["m1"])).unwrap();
    assert_eq!((hits(&conn, "alpha"), hits(&conn, "gamma")), (0, 1));
    integrity_ok(&conn);

    fts_index::remove_message(&conn, "a1", "m2").unwrap();
    assert_eq!((hits(&conn, "beta"), hits(&conn, "marker")), (0, 1));

    fts_index::remove_account(&conn, "a1").unwrap();
    assert_eq!(hits(&conn, "marker"), 0);
    let left: i64 = conn
        .query_row("SELECT count(*) FROM message_search", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);
    integrity_ok(&conn);
}

/// Put a populated database back into the pre-v15 shape (content-storing
/// FTS over raw bodies), then run the v15 step on it.
fn downgrade_then_migrate(conn: &Connection) {
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS message_search_ai; DROP TRIGGER IF EXISTS message_search_ad;
         DROP TRIGGER IF EXISTS message_search_au; DROP TABLE fts_messages;
         DELETE FROM message_search;
         CREATE VIRTUAL TABLE fts_messages USING fts5(message_id UNINDEXED, account_id UNINDEXED,
           subject, from_text, body, tokenize = 'porter unicode61');
         INSERT INTO fts_messages(rowid, message_id, account_id, subject, from_text, body)
         SELECT m.rowid, m.id, m.account_id, COALESCE(m.subject,''), '',
                (SELECT body FROM message_bodies b WHERE b.account_id=m.account_id
                   AND b.message_id=m.id LIMIT 1)
         FROM messages m;",
    )
    .unwrap();
    fts_index::migrate_v15(conn).unwrap();
}

#[test]
fn interrupted_rebuild_keeps_search_working_and_resumes() {
    let conn = test_conn();
    for (i, w) in ["apple", "banana", "cherry", "damson"].iter().enumerate() {
        add_message(
            &conn,
            &format!("m{i}"),
            "s",
            "text/html",
            &format!("<div>{w} fruit</div>"),
        );
    }
    downgrade_then_migrate(&conn);
    assert!(fts_index::rebuild_pending(&conn));
    assert_eq!(
        hits(&conn, "cherry"),
        1,
        "old index serves before any batch"
    );

    assert_eq!(
        fts_index::rebuild_step(&conn, 2).unwrap(),
        RebuildStep::Progress(2)
    );
    // Interrupted here: the old index still answers for what it held. Writes
    // in this window feed only the new index; they show up at the swap.
    assert_eq!(hits(&conn, "damson"), 1);
    add_message(&conn, "m9", "s", "text/plain", "elderberry fruit");
    fts_index::index_messages(&conn, "a1", &ids(&["m9"])).unwrap();
    set_body(&conn, "m0", "text/html", "<div>apricot fruit</div>");
    fts_index::index_messages(&conn, "a1", &ids(&["m0"])).unwrap();
    fts_index::remove_message(&conn, "a1", "m1").unwrap();
    assert_eq!(hits(&conn, "cherry"), 1);

    // Resume (as after an app restart): finish in small batches.
    let last = loop {
        let step = fts_index::rebuild_step(&conn, 2).unwrap();
        if !matches!(step, RebuildStep::Progress(_)) {
            break step;
        }
    };
    assert_eq!(last, RebuildStep::Swapped);
    assert!(!fts_index::rebuild_pending(&conn));
    assert_eq!(
        fts_index::rebuild_step(&conn, 2).unwrap(),
        RebuildStep::Idle
    );

    for (q, n) in [
        ("apricot", 1),
        ("cherry", 1),
        ("damson", 1),
        ("elderberry", 1),
        ("fruit", 4),
    ] {
        assert_eq!(hits(&conn, q), n, "{q} after swap");
    }
    assert_eq!(
        (
            hits(&conn, "apple"),
            hits(&conn, "banana"),
            hits(&conn, "div")
        ),
        (0, 0, 0)
    );
    let next: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='fts_messages_next'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(next, 0);
    integrity_ok(&conn);
    // Post-swap maintenance goes through the same triggers.
    fts_index::remove_message(&conn, "a1", "m9").unwrap();
    assert_eq!(hits(&conn, "elderberry"), 0);
    integrity_ok(&conn);
}
