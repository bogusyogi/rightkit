//! Lane L10 (2026-07-05) — search data layer: normalized recipients,
//! FTS-first candidates, prefix contact search.
//!
//! Covers:
//! - recipient operator tests: to:/cc:/bcc:/from: exact, prefix, and domain
//!   (`@domain`) forms, plus their negated (`-to:` etc.) counterparts.
//! - backfill-migration correctness: a v10 DB with pre-existing messages
//!   upgraded to v11 ends up with message_recipients rows matching the
//!   to/cc/bcc/from address counts parsed from the header-string columns.
//! - EXPLAIN QUERY PLAN: a `to:` query uses the message_recipients covering
//!   index (not a scan of messages.to_addrs), and a bare-text query uses the
//!   fts_candidate_sql candidate-CTE shape (not a threads-first scan).
//! - contact prefix search: the existing metadata_contact_tests regressions
//!   stay green (prefix semantics preserved) plus a new case proving a
//!   pure-substring match (no longer supported) is intentionally dropped.

use super::*;

const IDX_RECIPIENTS_LOOKUP: &str = "idx_message_recipients_lookup";
const IDX_RECIPIENTS_MESSAGE: &str = "idx_message_recipients_message";

fn ids(conn: &Connection, q: &str) -> Vec<String> {
    search_threads_scoped(conn, q, None, None, None)
        .unwrap()
        .into_iter()
        .map(|t| t.thread_id)
        .collect()
}

#[test]
fn to_cc_bcc_from_operators_match_exact_prefix_and_domain() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();

    let mut m1 = msg("t1", &["INBOX"], false, 10);
    m1.from_addr = "alice@example.com".into();
    m1.from_name = "Alice Anderson".into();
    m1.to_addrs = "Bob Jones <bob@example.com>, carol@other.org".into();
    m1.cc_addrs = "dave@example.com".into();

    let mut m2 = msg("t2", &["INBOX"], false, 20);
    m2.from_addr = "erin@example.com".into();
    m2.from_name = "Erin".into();
    m2.to_addrs = "frank@somewhere.net".into();

    apply_messages(&conn, "a@x.com", &[m1, m2]).unwrap();
    // bcc has no FetchedMessage field; seed directly the way a future
    // send-path writer would (mirrors search_operators_v1_parity).
    conn.execute(
        "INSERT INTO message_recipients(account_id, message_id, kind, addr_norm, name_norm)
         VALUES('a@x.com', 't1-10', 'bcc', 'gary@example.com', 'gary')",
        [],
    )
    .unwrap();

    // Exact address.
    assert_eq!(
        ids(&conn, "to:bob@example.com"),
        vec!["t1"],
        "to: exact address"
    );
    // Prefix (local-part).
    assert_eq!(ids(&conn, "to:bob"), vec!["t1"], "to: prefix on local-part");
    // Name prefix (bare term with no '@' also checks name_norm).
    assert_eq!(
        ids(&conn, "from:alice"),
        vec!["t1"],
        "from: prefix matches sender name"
    );
    assert_eq!(
        ids(&conn, "from:anderson"),
        Vec::<String>::new(),
        "from: prefix does not match mid-word (name_norm is prefix-matched, not substring)"
    );
    // cc: prefix.
    assert_eq!(ids(&conn, "cc:dave"), vec!["t1"], "cc: prefix");
    // bcc: exact.
    assert_eq!(
        ids(&conn, "bcc:gary@example.com"),
        vec!["t1"],
        "bcc: exact address"
    );
    // Domain form.
    let mut domain_hits = ids(&conn, "to:@example.com");
    domain_hits.sort();
    assert_eq!(
        domain_hits,
        vec!["t1"],
        "to:@domain matches every example.com recipient"
    );
    assert_eq!(
        ids(&conn, "to:@somewhere.net"),
        vec!["t2"],
        "to:@domain scopes to the right thread"
    );
    // No match.
    assert_eq!(
        ids(&conn, "to:nobody"),
        Vec::<String>::new(),
        "to: prefix miss returns empty"
    );

    // Negated forms.
    assert_eq!(
        ids(&conn, "in:inbox -to:bob"),
        vec!["t2"],
        "-to: excludes the matching thread"
    );
    assert_eq!(
        ids(&conn, "in:inbox -cc:dave"),
        vec!["t2"],
        "-cc: excludes the matching thread"
    );
    assert_eq!(
        ids(&conn, "in:inbox -bcc:gary"),
        vec!["t2"],
        "-bcc: excludes the matching thread"
    );
    assert_eq!(
        ids(&conn, "in:inbox -from:alice"),
        vec!["t2"],
        "-from: excludes the matching thread"
    );
}

#[test]
fn recipient_operators_survive_display_name_with_comma() {
    // "Doe, Jane" <jane@x.com> — a display name containing a comma must not
    // be split into two bogus address fragments by parse_address_list.
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    let mut m1 = msg("t1", &["INBOX"], false, 10);
    m1.to_addrs = "\"Doe, Jane\" <jane@x.com>, plain@x.com".into();
    apply_messages(&conn, "a@x.com", &[m1]).unwrap();

    assert_eq!(ids(&conn, "to:jane@x.com"), vec!["t1"]);
    assert_eq!(ids(&conn, "to:plain@x.com"), vec!["t1"]);
    // The comma-quoted name should not have produced a garbage third
    // recipient row that also happens to match some other query.
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM message_recipients
             WHERE account_id='a@x.com' AND message_id='t1-10' AND kind='to'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        n, 2,
        "exactly two recipients parsed out of the comma-name pair"
    );
}

#[test]
fn backfill_populates_message_recipients_from_existing_header_columns() {
    // Simulate an upgrade: build a v10 DB (pre-L10) with messages already on
    // disk via raw INSERT (mirrors production data written before this
    // migration existed), then run the full migrate() to v11 and assert the
    // backfill populated message_recipients to match.
    let conn = Connection::open_in_memory().expect("open v10 db");
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    conn.execute_batch(crate::db_schema::SCHEMA_V1).unwrap();
    conn.execute_batch(crate::db_schema::SCHEMA_V2).unwrap();
    conn.execute_batch(crate::db_schema::SCHEMA_V3).unwrap();
    conn.execute_batch(crate::db_schema::SCHEMA_V4).unwrap();
    conn.execute_batch(crate::db_schema::SCHEMA_V5).unwrap();
    conn.execute_batch(crate::db_schema::SCHEMA_V6).unwrap();
    add_column_if_missing_test_shim(&conn);
    conn.execute_batch(crate::db_schema::SCHEMA_V7).unwrap();
    conn.execute_batch(crate::db_schema::SCHEMA_V8).unwrap();
    conn.execute_batch(crate::db_schema::SCHEMA_V9).unwrap();
    conn.execute_batch(crate::db_schema::SCHEMA_V10).unwrap();
    conn.execute_batch("PRAGMA user_version = 10;").unwrap();

    conn.execute(
        "INSERT INTO accounts(id, display_name, created_at) VALUES('a@x.com','A',0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages
           (account_id, id, thread_id, from_addr, from_name, to_addrs, cc_addrs, bcc_addrs,
            subject, snippet, internal_date, label_ids, unread, has_attachments)
         VALUES('a@x.com','m1','t1','alice@x.com','Alice',
                'bob@x.com, carol@x.com','dave@x.com',NULL,
                'hi','snip',10,'[\"INBOX\"]',0,0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages
           (account_id, id, thread_id, from_addr, from_name, to_addrs, cc_addrs, bcc_addrs,
            subject, snippet, internal_date, label_ids, unread, has_attachments)
         VALUES('a@x.com','m2','t2','erin@x.com','Erin',
                NULL,NULL,NULL,
                'yo','snip',20,'[\"INBOX\"]',0,0)",
        [],
    )
    .unwrap();

    // Pre-condition: table does not exist yet at v10.
    let table_exists: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'message_recipients'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(table_exists, 0, "message_recipients must not exist pre-v11");

    migrate(&conn).expect("migrate v10 -> latest");
    let uv: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(uv, SCHEMA_VERSION);
    // L10 bumped SCHEMA_VERSION to 11 (message_recipients); L11 bumped it
    // again to 12 (size_estimate + rfc822_message_id index, db_schema.rs
    // SCHEMA_V12). This migrate() call runs both steps since the seeded DB
    // starts at v10 — assert the CURRENT version rather than pinning the
    // stale v11 literal, since that's a moving target every future schema
    // lane bumps.
    assert_eq!(
        SCHEMA_VERSION, 22,
        "label-first thread_labels index bumped SCHEMA_VERSION to 14, v15 external-content FTS, v16 agent gate, v17 agent send approvals, v18 agent forward and delete kinds, v19 decline cooldown and attachment digest, v20 semantic pipeline key, v21 agent attach approvals, v22 job-queue outbox and pending ops"
    );

    // m1: from(1) + to(2) + cc(1) = 4 rows. m2: from(1) = 1 row. Total 5.
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM message_recipients", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        total, 5,
        "backfill count must match parsed to+cc+bcc+from recipients"
    );

    let m1_to: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT addr_norm FROM message_recipients
                 WHERE account_id='a@x.com' AND message_id='m1' AND kind='to' ORDER BY addr_norm",
            )
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        m1_to,
        vec!["bob@x.com".to_string(), "carol@x.com".to_string()]
    );
}

/// v6->v7 needs threads.important added before SCHEMA_V7's index references
/// it, mirroring migrate()'s own `current < 7` branch (see db/migrations.rs).
fn add_column_if_missing_test_shim(conn: &Connection) {
    conn.execute_batch("ALTER TABLE threads ADD COLUMN important INTEGER NOT NULL DEFAULT 0;")
        .unwrap();
}

#[test]
fn backfill_is_idempotent_on_a_second_migrate_call() {
    let conn = test_conn(); // already at latest SCHEMA_VERSION via migrate()
    upsert_account(&conn, "a@x.com").unwrap();
    let mut m1 = msg("t1", &["INBOX"], false, 10);
    m1.to_addrs = "bob@x.com".into();
    apply_messages(&conn, "a@x.com", &[m1]).unwrap();

    let before: i64 = conn
        .query_row("SELECT COUNT(*) FROM message_recipients", [], |r| r.get(0))
        .unwrap();
    migrate(&conn).expect("second migrate is a no-op");
    let after: i64 = conn
        .query_row("SELECT COUNT(*) FROM message_recipients", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        before, after,
        "second migrate must not duplicate or touch rows"
    );
}

#[test]
fn explain_to_query_uses_message_recipients_covering_index() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    let mut m1 = msg("t1", &["INBOX"], false, 10);
    m1.to_addrs = "bob@x.com".into();
    apply_messages(&conn, "a@x.com", &[m1]).unwrap();

    // Mirror the exact correlated-EXISTS shape recipient_predicate builds.
    let sql = "SELECT 1 FROM messages m
               WHERE m.account_id = 'a@x.com'
                 AND EXISTS(SELECT 1 FROM message_recipients mr
                              WHERE mr.account_id = m.account_id
                                AND mr.message_id = m.id
                                AND mr.kind = 'to'
                                AND mr.addr_norm LIKE 'bob%')";
    let plan: Vec<String> = {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        stmt.query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let plan_blob = plan.join("\n");
    eprintln!("EXPLAIN QUERY PLAN for to: query:\n{plan_blob}");
    // The planner may satisfy the correlated EXISTS via either
    // idx_message_recipients_lookup (account_id, addr_norm, kind) or
    // idx_message_recipients_message (account_id, message_id) — both are
    // indexed point-lookups on message_recipients; which one SQLite picks
    // depends on its cost estimate for this tiny per-message EXISTS. Either
    // is the win this lane targets: the important assertion is that
    // message_recipients is reached via SOME index, never a table scan.
    assert!(
        plan_blob.contains(IDX_RECIPIENTS_LOOKUP) || plan_blob.contains(IDX_RECIPIENTS_MESSAGE),
        "to: query must use an indexed lookup on message_recipients (got:\n{plan_blob})"
    );
    assert!(
        !plan_blob
            .to_ascii_uppercase()
            .contains("SCAN MESSAGE_RECIPIENTS"),
        "to: query must not fall back to a message_recipients table scan (got:\n{plan_blob})"
    );
    assert!(
        !plan_blob.to_ascii_uppercase().contains("SCAN MESSAGES"),
        "to: query must not fall back to a messages table scan (got:\n{plan_blob})"
    );
}

#[test]
fn explain_bare_text_query_uses_fts_candidate_cte_not_threads_scan() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    let mut m1 = msg("t1", &["INBOX"], false, 10);
    m1.subject = "Needle receipt".into();
    apply_search_messages(&conn, "a@x.com", &[m1]).unwrap();

    // Mirror the fts_candidate_sql cand-CTE shape for a bare text query.
    let sql = "WITH cand AS (
                 SELECT DISTINCT m.account_id, m.thread_id
                 FROM messages m
                 WHERE m.id IN (SELECT message_id FROM fts_messages WHERE fts_messages MATCH 'needle')
               )
               SELECT * FROM cand";
    let plan: Vec<String> = {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        stmt.query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let plan_blob = plan.join("\n");
    eprintln!("EXPLAIN QUERY PLAN for bare-text candidate query:\n{plan_blob}");
    assert!(
        !plan_blob.to_ascii_uppercase().contains("SCAN THREADS"),
        "candidate-first text query must not scan threads (got:\n{plan_blob})"
    );

    // Functional check: search_threads for a bare term actually uses the
    // candidate path and returns the right thread (regression, not just plan).
    let hits = ids(&conn, "needle");
    assert_eq!(hits, vec!["t1"]);
}

#[test]
fn fts_candidate_path_still_applies_account_and_label_scope() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    upsert_account(&conn, "b@x.com").unwrap();
    let mut a1 = msg("shared", &["INBOX"], false, 10);
    a1.subject = "needle in account a".into();
    let mut b1 = msg("shared", &["INBOX"], false, 10);
    b1.id = "b-msg".into();
    b1.subject = "needle in account b".into();
    apply_search_messages(&conn, "a@x.com", &[a1]).unwrap();
    apply_search_messages(&conn, "b@x.com", &[b1]).unwrap();

    let a_only = search_threads_scoped(&conn, "needle", Some("a@x.com"), None, None).unwrap();
    assert_eq!(a_only.len(), 1);
    assert_eq!(a_only[0].account_id, "a@x.com");

    let all = search_threads_scoped(&conn, "needle", None, None, None).unwrap();
    assert_eq!(
        all.len(),
        2,
        "unscoped candidate search covers both accounts"
    );
}

// --- Contact prefix search regressions (P22) ---

#[test]
fn contact_prefix_search_preserves_existing_regression_matches() {
    let mut conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
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
        ],
    )
    .unwrap();

    let hits = search_contacts_local(&conn, "a@x.com", "ali", 12).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].email, "alice@example.com");

    let hits = search_contacts_local(&conn, "a@x.com", "bob@", 12).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].email, "bob@example.com");
}

#[test]
fn contact_prefix_search_no_longer_matches_pure_mid_string_substring() {
    // Documented semantic narrowing (see search_contacts_local doc comment):
    // "mith" is a mid-string substring of "Smith", not a prefix of the name
    // or email. The old `%term%` LIKE would have matched it; the new
    // prefix-indexed lookup intentionally does not.
    let mut conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    cache_contacts(
        &mut conn,
        "a@x.com",
        &[CachedContact {
            name: "Alice Smith".into(),
            email: "alice@example.com".into(),
            photo_url: String::new(),
        }],
    )
    .unwrap();
    assert!(
        search_contacts_local(&conn, "a@x.com", "mith", 12)
            .unwrap()
            .is_empty(),
        "mid-string substring is intentionally no longer matched"
    );
    // But a prefix of the surname still works because name_lower LIKE
    // 'smith%' would only match if surname led the string — Gmail-style
    // contact pickers key off leading tokens, not mid-word matches.
}

// Regression for the bug that made ensure_runtime_schema's name_lower guard
// throw "duplicate column name" on every second call: this bundled SQLite
// build omits STORED generated columns from `PRAGMA table_info` entirely
// (only `table_xinfo` lists them), so a naive `column_exists`-style check
// against `table_info` always reports the column missing and re-runs the
// ALTER. Pin the underlying pragma behavior directly so a future SQLite
// upgrade that changes this silently fails the test instead of silently
// reintroducing the bug.
#[test]
fn pragma_table_info_omits_stored_generated_column_but_table_xinfo_includes_it() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE t(name TEXT); ALTER TABLE t ADD COLUMN name_lower TEXT GENERATED ALWAYS AS (lower(name)) STORED;",
    )
    .unwrap();

    let table_info_names: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(t)").unwrap();
        stmt.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert!(
        !table_info_names.contains(&"name_lower".to_string()),
        "documents current bundled-SQLite behavior: table_info omits STORED \
         generated columns (got: {table_info_names:?}). If this now fails, \
         SQLite's behavior changed — column_exists() may be usable for \
         generated columns again, but generated_column_exists() via \
         table_xinfo remains correct either way."
    );

    let table_xinfo_names: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA table_xinfo(t)").unwrap();
        stmt.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert!(
        table_xinfo_names.contains(&"name_lower".to_string()),
        "table_xinfo must list the generated column (got: {table_xinfo_names:?})"
    );
}

#[test]
fn contacts_cache_name_lower_generated_column_is_indexed() {
    let conn = test_conn();
    let partials: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA index_list(contacts_cache)").unwrap();
        stmt.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert!(
        partials.contains(&"idx_contacts_name_lower".to_string()),
        "contacts_cache must have an index on the generated name_lower column (got: {partials:?})"
    );
}
