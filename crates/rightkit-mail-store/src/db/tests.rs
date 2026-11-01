use super::*;
use rightkit_google_apis::gmail::FetchedAttachment;

fn seed_account(conn: &Connection, email: &str) {
    upsert_account(conn, email).unwrap();
    conn.execute(
        "INSERT INTO messages
               (account_id, id, thread_id, from_addr, from_name, subject,
                snippet, internal_date, label_ids, unread, has_attachments, auth_results)
             VALUES(?1, 'm1', 't1', 's@x.com', 'S', 'hi', 'snip', 1, '[\"INBOX\"]', 1, 0, NULL)",
        [email],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO message_bodies(account_id, message_id, mime_type, body)
             VALUES(?1, 'm1', 'text/plain', 'body')",
        [email],
    )
    .unwrap();
}

// Safety rail #9: removing an account is purely local and scoped. The remove
// path constructs no HTTP client at all (verifiable by reading
// remove_account / commands::remove_account — they touch only SQLite + the
// Credential Manager), so it cannot call a Google delete endpoint. This test
// proves it deletes only the target account's rows, never a sibling's.

fn msg(thread: &str, labels: &[&str], unread: bool, date: i64) -> FetchedMessage {
    FetchedMessage {
        id: format!("{thread}-{date}"),
        thread_id: thread.to_string(),
        from_name: "S".into(),
        from_addr: "s@x.com".into(),
        to_addrs: "me@x.com".into(),
        cc_addrs: String::new(),
        reply_to: None,
        list_unsubscribe: None,
        auth_results: None,
        subject: "subj".into(),
        snippet: "snip".into(),
        internal_date: date,
        unread,
        label_ids: labels.iter().map(|s| s.to_string()).collect(),
        body_plain: Some("body".into()),
        body_html: None,
        has_attachments: false,
        attachments: Vec::new(),
        size_estimate: None,
        rfc822_message_id: None,
    }
}

mod account_lifecycle_tests;
mod apply_messages_incremental_tests;
mod checkpoint_cadence_tests;
mod chunked_rollup_tests;
mod fts_external_tests;
mod fts_rowid_tests;
mod label_sync_tests;
mod metadata_contact_tests;
mod search_data_layer_tests;
mod sync_cursor_tests;
mod sync_lease_tests;
