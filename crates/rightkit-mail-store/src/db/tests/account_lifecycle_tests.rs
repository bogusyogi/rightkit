use super::*;

/// Seed one full row per no-FK table `remove_account_tx` deletes, so the
/// "everything gone" assertion actually exercises all five DELETEs (the
/// original `seed_account` only inserts a message + a body).
fn seed_full_account(conn: &Connection, email: &str) {
    seed_account(conn, email);
    conn.execute(
        "INSERT INTO attachments(account_id, message_id, attachment_id, filename, mime_type, size)
         VALUES(?1, 'm1', 'att1', 'f.txt', 'text/plain', 10)",
        [email],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO message_search(rowid, message_id, account_id, subject, from_text, body)
         SELECT rowid, id, account_id, subject, from_name, 'body' FROM messages
         WHERE account_id = ?1 AND id = 'm1'",
        [email],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ai_summaries(account_id, thread_id, provider, model, summary, created_at)
         VALUES(?1, 't1', 'p', 'm', 's', 1)",
        [email],
    )
    .unwrap();
}

/// L7: `remove_account_tx` runs all five DELETEs (+ the removal audit row)
/// in one transaction. Insert one row in every table it touches, remove,
/// assert every one of them is gone for the target account and untouched
/// for a sibling.
#[test]
fn remove_account_tx_removes_every_table_atomically() {
    let mut conn = test_conn();
    seed_full_account(&conn, "a@x.com");
    seed_full_account(&conn, "b@x.com");

    remove_account_tx(&mut conn, "a@x.com").unwrap();

    let count = |table: &str, column: &str, id: &str| -> i64 {
        conn.query_row(
            &format!("SELECT count(*) FROM {table} WHERE {column} = ?1"),
            [id],
            |r| r.get(0),
        )
        .unwrap()
    };

    // `accounts` keys on `id`; every other table here keys on `account_id`.
    for id in ["a@x.com", "b@x.com"] {
        let expected = if id == "a@x.com" { 0 } else { 1 };
        assert_eq!(
            count("accounts", "id", id),
            expected,
            "accounts row for {id}"
        );
        for table in [
            "messages",
            "message_bodies",
            "attachments",
            "fts_messages",
            "ai_summaries",
        ] {
            assert_eq!(
                count(table, "account_id", id),
                expected,
                "{table} rows for {id}"
            );
        }
    }

    let audit_rows: i64 = conn
        .query_row(
            "SELECT count(*) FROM audit_log WHERE kind = 'account' AND account_id = 'a@x.com'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        audit_rows, 1,
        "removal audit row written in the same transaction"
    );
}

/// A failure partway through the transaction rolls back everything,
/// including the accounts DELETE that ran first — no partial/orphaned
/// removal. Simulated by dropping the `ai_summaries` table so its DELETE
/// errors after the four earlier statements already ran inside the txn.
#[test]
fn remove_account_tx_rolls_back_on_failure() {
    let mut conn = test_conn();
    seed_full_account(&conn, "a@x.com");
    conn.execute("DROP TABLE ai_summaries", []).unwrap();

    let result = remove_account_tx(&mut conn, "a@x.com");
    assert!(result.is_err(), "forced failure should surface as an error");

    let acct: i64 = conn
        .query_row(
            "SELECT count(*) FROM accounts WHERE id = 'a@x.com'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        acct, 1,
        "rollback keeps the account row (no partial removal)"
    );

    let msgs: i64 = conn
        .query_row(
            "SELECT count(*) FROM messages WHERE account_id = 'a@x.com'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        msgs, 1,
        "rollback keeps messages too — the whole txn undone"
    );
}

#[test]
fn remove_account_is_local_and_scoped() {
    let mut conn = test_conn();
    seed_account(&conn, "a@x.com");
    seed_account(&conn, "b@x.com");

    remove_account_tx(&mut conn, "a@x.com").unwrap();

    let acct = |id: &str| -> i64 {
        conn.query_row("SELECT count(*) FROM accounts WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
    };
    let msgs = |id: &str| -> i64 {
        conn.query_row(
            "SELECT count(*) FROM messages WHERE account_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    };
    let bodies = |id: &str| -> i64 {
        conn.query_row(
            "SELECT count(*) FROM message_bodies WHERE account_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    };

    assert_eq!(acct("a@x.com"), 0, "target account row removed");
    assert_eq!(msgs("a@x.com"), 0, "target messages cascade-removed");
    assert_eq!(bodies("a@x.com"), 0, "target bodies removed");
    assert_eq!(acct("b@x.com"), 1, "sibling account untouched");
    assert_eq!(msgs("b@x.com"), 1, "sibling messages untouched");
    assert_eq!(bodies("b@x.com"), 1, "sibling bodies untouched");
}
