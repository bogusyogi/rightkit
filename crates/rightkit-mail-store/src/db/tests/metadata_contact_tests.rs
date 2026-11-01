use super::*;

#[test]
fn metadata_upsert_does_not_clear_known_attachment_or_body() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    let mut full = msg("t1", &["INBOX"], false, 10);
    full.id = "m1".into();
    full.has_attachments = true;
    full.body_plain = Some("full body".into());
    apply_messages(&conn, "a@x.com", &[full.clone()]).unwrap();

    let mut metadata = full;
    metadata.has_attachments = false;
    metadata.body_plain = None;
    metadata.body_html = None;
    metadata.snippet = "metadata refresh".into();
    apply_messages(&conn, "a@x.com", &[metadata]).unwrap();

    let has_attachments: i64 = conn
        .query_row(
            "SELECT has_attachments FROM messages WHERE account_id = 'a@x.com' AND id = 'm1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let body_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM message_bodies WHERE account_id = 'a@x.com' AND message_id = 'm1'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    assert_eq!(has_attachments, 1);
    assert_eq!(body_count, 1);
}

// The unread-coverage heal must clear a stale history cursor exactly once,
// so the next sync runs a full pass that now includes the UNREAD folder —
// then never wipe a freshly-written cursor again (idempotent).
#[test]
fn ensure_runtime_schema_resets_cursor_once() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    migrate(&conn).unwrap();
    conn.execute(
        "INSERT INTO accounts(id, display_name, color, created_at) VALUES('a@x.com','a','#fff',1)",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO sync_state(account_id) VALUES('a@x.com')", [])
        .unwrap();
    set_history_id(&conn, "a@x.com", "100").unwrap();

    // First run heals: the stale cursor is cleared → next sync is full.
    ensure_runtime_schema(&conn).unwrap();
    assert_eq!(
        get_history_id(&conn, "a@x.com").unwrap(),
        None,
        "stale cursor cleared once"
    );

    // A cursor written by the subsequent full sync must survive later opens.
    set_history_id(&conn, "a@x.com", "200").unwrap();
    ensure_runtime_schema(&conn).unwrap();
    assert_eq!(
        get_history_id(&conn, "a@x.com").unwrap(),
        Some("200".into()),
        "heal is one-time; later cursors are not wiped"
    );
}

// M18: the cipher-key guard accepts base64 (our key shape) and rejects
// anything with a quote or unexpected byte before it reaches PRAGMA.
#[test]
fn key_is_safe_accepts_base64_rejects_injection() {
    assert!(key_is_safe("YWJjZA=="));
    assert!(key_is_safe("aB3+/x9Z"));
    assert!(!key_is_safe("")); // empty
    assert!(!key_is_safe("abc'; DROP TABLE x;--")); // quote / SQL
    assert!(!key_is_safe("abc def")); // space
    assert!(!key_is_safe("abc\n")); // control char
}

// M12: local contact cache feeds offline autocomplete. Caching is a full
// refresh (delete-then-insert); local search matches name OR email substring
// (case-insensitive), with prefix matches ranked first.
#[test]
fn contacts_cache_local_search() {
    let mut conn = test_conn();
    conn.execute(
        "INSERT INTO accounts(id, display_name, color, created_at) VALUES('a@x.com','a','#fff',1)",
        [],
    )
    .unwrap();

    assert!(!has_cached_contacts(&conn, "a@x.com").unwrap());

    cache_contacts(
        &mut conn,
        "a@x.com",
        &[
            CachedContact {
                name: "Alice Smith".into(),
                email: "alice@example.com".into(),
                photo_url: String::new(),
            },
            CachedContact {
                name: "Bob Jones".into(),
                email: "BOB@example.com".into(),
                photo_url: String::new(),
            },
            CachedContact {
                name: "Carol".into(),
                email: "".into(),
                photo_url: String::new(),
            }, // empty email skipped
        ],
    )
    .unwrap();

    assert!(has_cached_contacts(&conn, "a@x.com").unwrap());

    // Name substring, case-insensitive.
    let hits = search_contacts_local(&conn, "a@x.com", "ali", 12).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].email, "alice@example.com");

    // Email match, and emails are stored lowercased.
    let hits = search_contacts_local(&conn, "a@x.com", "bob@", 12).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].email, "bob@example.com");

    // Empty-email contact was skipped.
    let hits = search_contacts_local(&conn, "a@x.com", "carol", 12).unwrap();
    assert!(hits.is_empty());

    // Refresh replaces the whole set for the account.
    cache_contacts(
        &mut conn,
        "a@x.com",
        &[CachedContact {
            name: "Dave".into(),
            email: "dave@example.com".into(),
            photo_url: String::new(),
        }],
    )
    .unwrap();
    assert!(search_contacts_local(&conn, "a@x.com", "ali", 12)
        .unwrap()
        .is_empty());
    assert_eq!(
        search_contacts_local(&conn, "a@x.com", "dave", 12)
            .unwrap()
            .len(),
        1
    );

    // Empty query returns nothing.
    assert!(search_contacts_local(&conn, "a@x.com", "  ", 12)
        .unwrap()
        .is_empty());
}

// The risky part of 0.4: upgrading an existing (v1) DB must backfill
// thread_labels from the messages already on disk, or every folder renders
// empty. Simulate a v1 DB with messages but no thread_labels, then migrate.
