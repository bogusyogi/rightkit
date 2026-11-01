use super::*;

#[test]
fn apply_labels_stores_system_and_custom() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    let labels = vec![
        LabelInfo {
            id: "INBOX".into(),
            name: "INBOX".into(),
            label_type: "system".into(),
            color: None,
        },
        LabelInfo {
            id: "Label_42".into(),
            name: "Orders".into(),
            label_type: "user".into(),
            color: None,
        },
    ];
    apply_labels(&conn, "a@x.com", &labels).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM labels WHERE account_id='a@x.com'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 2);
    let name: String = conn
        .query_row(
            "SELECT name FROM labels WHERE account_id='a@x.com' AND id='Label_42'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(name, "Orders", "custom label is catalogued, not dropped");
}

#[test]
fn custom_labels_preserved_in_thread_labels() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    // A synced INBOX message that also carries a custom label.
    let msgs = vec![msg("t1", &["INBOX", "Label_42"], true, 10)];
    apply_messages(&conn, "a@x.com", &msgs).unwrap();
    let has_custom: i64 = conn
        .query_row(
            "SELECT count(*) FROM thread_labels
                 WHERE account_id='a@x.com' AND thread_id='t1' AND label='Label_42'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has_custom, 1, "custom label preserved on a synced message");
}
