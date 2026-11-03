use super::*;

#[test]
fn search_filename_scoped_by_account() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    upsert_account(&conn, "b@x.com").unwrap();
    // Same thread_id "same" appears in both accounts. Account a has an
    // attachment named "invoice.pdf"; account b has the same id but no
    // attachment. The filename filter must NOT leak across accounts.
    let mut a = msg("same", &["INBOX"], false, 10);
    a.id = "a-msg".into();
    a.has_attachments = true;
    a.attachments = vec![FetchedAttachment {
        attachment_id: "att-a".into(),
        filename: "invoice.pdf".into(),
        mime_type: "application/pdf".into(),
        size: 1024,
    }];
    let mut b = msg("same", &["INBOX"], false, 10);
    b.id = "b-msg".into();
    apply_messages(&conn, "a@x.com", &[a]).unwrap();
    apply_messages(&conn, "b@x.com", &[b]).unwrap();

    // Without an account scope, the all-account search returns only the
    // threads with attachments (account a). Account b has no attachments so
    // its row is excluded by the per-message EXISTS.
    let unscoped_a =
        search_threads_scoped(&conn, "has:attachment", Some("a@x.com"), None, None).unwrap();
    assert!(
        unscoped_a.iter().any(|t| t.thread_id == "same"),
        "a@x.com has the attachment"
    );
    let unscoped_b =
        search_threads_scoped(&conn, "has:attachment", Some("b@x.com"), None, None).unwrap();
    assert!(
        !unscoped_b.iter().any(|t| t.thread_id == "same"),
        "b@x.com has no attachment, no leak"
    );
}

#[test]
fn search_fts_special_chars_dont_throw() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    let mut m1 = msg("t1", &["INBOX"], false, 20);
    m1.subject = "appeal/x appeal-2026".into();
    apply_messages(&conn, "a@x.com", &[m1]).unwrap();
    assert!(search_threads_scoped(&conn, "appeal/x", None, None, None).is_ok());
    assert!(search_threads_scoped(&conn, "appeal-2026", None, None, None).is_ok());
}
