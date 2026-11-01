//! Label, read and star changes arrive as re-fetched messages. They must not
//! cost a message its vector; only a subject/body change or a deletion does.
//! In-memory database and the fake embedder only.

use super::embedder::STORED_DIM;
use super::index::run_index_step;
use super::index::test_support::FakeEmbedder;
use super::storage::get_index_state;
use crate::db::{apply_delta, apply_messages, test_conn, upsert_account};
use rightkit_db::rusqlite::Connection;
use rightkit_google_apis::gmail::FetchedMessage;

const ACCOUNT: &str = "a@x.com";

fn fetched(labels: &[&str], unread: bool, subject: &str, body: &str) -> FetchedMessage {
    FetchedMessage {
        id: "m1".into(),
        thread_id: "t1".into(),
        from_name: "S".into(),
        from_addr: "s@x.com".into(),
        to_addrs: "me@x.com".into(),
        cc_addrs: String::new(),
        reply_to: None,
        list_unsubscribe: None,
        auth_results: None,
        subject: subject.into(),
        snippet: "snip".into(),
        internal_date: 1000,
        unread,
        label_ids: labels.iter().map(|s| s.to_string()).collect(),
        body_plain: Some(body.into()),
        body_html: None,
        has_attachments: false,
        attachments: Vec::new(),
        size_estimate: None,
        rfc822_message_id: None,
    }
}

fn indexed_conn() -> Connection {
    let conn = test_conn();
    upsert_account(&conn, ACCOUNT).unwrap();
    apply_messages(
        &conn,
        ACCOUNT,
        &[fetched(&["INBOX"], true, "budget", "numbers")],
    )
    .unwrap();
    let e = FakeEmbedder {
        dim: STORED_DIM,
        model_id: "fake-v1",
    };
    assert_eq!(run_index_step(&conn, &conn, ACCOUNT, &e).unwrap(), 1);
    conn
}

fn vectors(conn: &Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM message_vec", [], |r| r.get(0))
        .unwrap()
}

fn assert_kept(conn: &Connection) {
    assert_eq!(vectors(conn), 1, "vector kept");
    let st = get_index_state(conn, ACCOUNT).unwrap();
    assert!(st.done, "index stays finished");
    assert_eq!(st.indexed_count, 1);
}

#[test]
fn label_read_and_star_changes_keep_the_vector() {
    let conn = indexed_conn();
    for (labels, unread) in [
        (&["INBOX", "STARRED"][..], true),
        (&["INBOX", "STARRED"][..], false),
        (&["Label_7"][..], false),
        (&[][..], false),
    ] {
        let m = fetched(labels, unread, "budget", "numbers");
        apply_delta(&conn, ACCOUNT, &[m], &[]).unwrap();
        assert_kept(&conn);
    }
}

#[test]
fn body_change_requeues_the_message() {
    // A stored subject never changes on a re-fetch (Gmail messages are
    // immutable and the upsert keeps it), so only a body change counts.
    for (subject, body) in [("budget", "new numbers"), ("budget", "numbers, revised")] {
        let conn = indexed_conn();
        let m = fetched(&["INBOX"], true, subject, body);
        apply_delta(&conn, ACCOUNT, &[m], &[]).unwrap();
        assert_eq!(
            vectors(&conn),
            0,
            "stale vector dropped for {subject}/{body}"
        );
        let st = get_index_state(&conn, ACCOUNT).unwrap();
        assert!(!st.done);
        let e = FakeEmbedder {
            dim: STORED_DIM,
            model_id: "fake-v1",
        };
        assert_eq!(run_index_step(&conn, &conn, ACCOUNT, &e).unwrap(), 1);
        assert_kept(&conn);
    }
}

#[test]
fn deleting_a_message_drops_its_vector() {
    let conn = indexed_conn();
    apply_delta(&conn, ACCOUNT, &[], &["m1".to_string()]).unwrap();
    assert_eq!(vectors(&conn), 0);
}

#[test]
fn new_message_after_a_finished_index_is_picked_up() {
    let conn = indexed_conn();
    let mut second = fetched(&["INBOX"], true, "lunch", "tacos");
    second.id = "m2".into();
    second.thread_id = "t2".into();
    apply_delta(&conn, ACCOUNT, &[second], &[]).unwrap();
    assert!(!get_index_state(&conn, ACCOUNT).unwrap().done);
    let e = FakeEmbedder {
        dim: STORED_DIM,
        model_id: "fake-v1",
    };
    assert_eq!(run_index_step(&conn, &conn, ACCOUNT, &e).unwrap(), 1);
    assert_eq!(vectors(&conn), 2);
}
