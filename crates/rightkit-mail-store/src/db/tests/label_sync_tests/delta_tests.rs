use super::*;

#[test]
fn apply_delta_updates_labels_and_deletes() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    apply_messages(
        &conn,
        "a@x.com",
        &[msg("t1", &["INBOX"], false, 10), {
            let mut m = msg("t2", &["INBOX"], false, 20);
            m.has_attachments = true;
            m.attachments = vec![FetchedAttachment {
                attachment_id: "att-t2".into(),
                filename: "deleted.pdf".into(),
                mime_type: "application/pdf".into(),
                size: 42,
            }];
            m
        }],
    )
    .unwrap();
    assert_eq!(list_threads(&conn, None, "INBOX").unwrap().len(), 2);

    // Delta: t1's message archived (INBOX -> SENT), t2's message deleted.
    let changed = vec![msg("t1", &["SENT"], false, 10)];
    let deleted = vec!["t2-20".to_string()]; // msg() id == "{thread}-{date}"
    apply_delta(&conn, "a@x.com", &changed, &deleted).unwrap();

    assert!(
        list_threads(&conn, None, "INBOX").unwrap().is_empty(),
        "t1 archived out of INBOX, t2 deleted"
    );
    let sent = list_threads(&conn, None, "SENT").unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].thread_id, "t1", "t1 now shows under SENT");
    let stale_attachments: i64 = conn
        .query_row(
            "SELECT count(*) FROM attachments WHERE account_id='a@x.com' AND message_id='t2-20'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        stale_attachments, 0,
        "deleted messages do not leave attachment rows"
    );
}

#[test]
fn history_id_roundtrip() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    assert_eq!(get_history_id(&conn, "a@x.com").unwrap(), None);
    set_history_id(&conn, "a@x.com", "12345").unwrap();
    assert_eq!(
        get_history_id(&conn, "a@x.com").unwrap(),
        Some("12345".to_string())
    );
}

#[test]
fn empty_delta_advances_cursor_without_audit() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    let work =
        rightkit_google_apis::gmail::SyncWork::Delta(rightkit_google_apis::gmail::DeltaOutcome {
            changed: vec![],
            deleted: vec![],
            new_history_id: Some("500".into()),
            latest_history_id: Some("500".into()),
            stale: false,
            failed: 0,
            failed_ids: vec![],
        });
    apply_sync_work(&conn, "a@x.com", work).unwrap();
    assert_eq!(
        get_history_id(&conn, "a@x.com").unwrap(),
        Some("500".into()),
        "cursor advances on a clean empty delta"
    );
    let audits: i64 = conn
        .query_row(
            "SELECT count(*) FROM audit_log WHERE account_id='a@x.com' AND kind='sync'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(audits, 0, "no audit row for a no-op delta");
}

#[test]
fn delta_with_failed_fetch_holds_cursor() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    set_history_id(&conn, "a@x.com", "100").unwrap();
    // sync_delta clamps new_history_id to None when any fetch failed.
    let work =
        rightkit_google_apis::gmail::SyncWork::Delta(rightkit_google_apis::gmail::DeltaOutcome {
            changed: vec![msg("t1", &["INBOX"], false, 10)],
            deleted: vec![],
            new_history_id: None,
            latest_history_id: Some("200".into()),
            stale: false,
            failed: 1,
            failed_ids: vec!["missing-later".into()],
        });
    apply_sync_work(&conn, "a@x.com", work).unwrap();
    // The fetched change is applied...
    assert_eq!(list_threads(&conn, None, "INBOX").unwrap().len(), 1);
    // ...but the cursor is held so the next delta re-attempts the failed id.
    assert_eq!(
        get_history_id(&conn, "a@x.com").unwrap(),
        Some("100".into()),
        "cursor not advanced while a change is unreconciled"
    );
}

#[test]
fn delta_repeated_failed_fetch_advances_after_threshold() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    set_history_id(&conn, "a@x.com", "100").unwrap();

    for attempt in 1..=2 {
        let work = rightkit_google_apis::gmail::SyncWork::Delta(
            rightkit_google_apis::gmail::DeltaOutcome {
                changed: vec![],
                deleted: vec![],
                new_history_id: None,
                latest_history_id: Some("200".into()),
                stale: false,
                failed: 1,
                failed_ids: vec!["stuck-msg".into()],
            },
        );
        apply_sync_work(&conn, "a@x.com", work).unwrap();
        assert_eq!(
            get_history_id(&conn, "a@x.com").unwrap(),
            Some("100".into()),
            "attempt {attempt}: cursor stays pinned while retry budget remains"
        );
    }

    let work =
        rightkit_google_apis::gmail::SyncWork::Delta(rightkit_google_apis::gmail::DeltaOutcome {
            changed: vec![],
            deleted: vec![],
            new_history_id: None,
            latest_history_id: Some("200".into()),
            stale: false,
            failed: 1,
            failed_ids: vec!["stuck-msg".into()],
        });
    apply_sync_work(&conn, "a@x.com", work).unwrap();
    assert_eq!(
        get_history_id(&conn, "a@x.com").unwrap(),
        Some("200".into()),
        "third identical failure is skipped so the account can keep syncing"
    );
}
