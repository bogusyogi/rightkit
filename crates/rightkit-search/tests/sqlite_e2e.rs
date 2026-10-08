//! End-to-end: queries parsed and compiled by rightkit-search execute against a
//! real on-disk SQLite database (FTS5 external-content table) and are validated
//! by the rows that come back.

use rightkit_search::snippet::{fts_snippet_expr, split_highlights};
use rightkit_search::{Binding, CompileError, ParseError, Query, Schema, ValueKind};
use rusqlite::{params_from_iter, Connection};

fn schema() -> Schema {
    Schema::new()
        .fts("fts_messages", "m.rowid")
        .bind("from", Binding::Like("m.sender"))
        .bind("subject", Binding::Like("m.subject"))
        .bind("label", Binding::Eq("m.label"))
        .bind("larger", Binding::Cmp { column: "m.size", cmp: ">=", kind: ValueKind::SizeBytes })
        .bind("smaller", Binding::Cmp { column: "m.size", cmp: "<", kind: ValueKind::SizeBytes })
        .bind("after", Binding::Cmp { column: "m.date_ms", cmp: ">=", kind: ValueKind::DateMs })
        .bind("before", Binding::Cmp { column: "m.date_ms", cmp: "<", kind: ValueKind::DateMs })
        .bind("newer", Binding::Cmp { column: "m.date_ms", cmp: ">=", kind: ValueKind::RelativeMs })
        .bind("rfc822msgid", Binding::Cmp { column: "m.msgid", cmp: "=", kind: ValueKind::BareId })
        .bind(
            "filename",
            Binding::Sql {
                sql: "m.id IN (SELECT message_id FROM attachments WHERE filename LIKE ? ESCAPE '\\')",
                kind: ValueKind::Contains,
            },
        )
        .bind("is", Binding::Flags(&[("unread", "m.unread = 1"), ("read", "m.unread = 0")]))
        .bind("has", Binding::Flags(&[("attachment", "EXISTS (SELECT 1 FROM attachments a WHERE a.message_id = m.id)")]))
}

// 2024-03-01 and 2024-04-01 in epoch ms.
const MAR1: i64 = 1_709_251_200_000;
const APR1: i64 = 1_711_929_600_000;

fn fixture() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let conn = Connection::open(dir.path().join("mail.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE messages(id TEXT PRIMARY KEY, sender TEXT, subject TEXT, body TEXT, label TEXT,
                               size INTEGER, date_ms INTEGER, unread INTEGER, msgid TEXT);
         CREATE TABLE attachments(message_id TEXT, filename TEXT);
         CREATE VIRTUAL TABLE fts_messages USING fts5(subject, body, content='messages', content_rowid='rowid');",
    )
    .unwrap();
    let rows = [
        (
            "m1",
            "alice@acme.com",
            "March invoice",
            "Please find the invoice for March attached. Payment due in 30 days.",
            "INBOX",
            120_000,
            MAR1,
            1,
            "id1@acme",
        ),
        (
            "m2",
            "bob@example.org",
            "Lunch on Friday",
            "Want to grab lunch on Friday? The new ramen place is open.",
            "INBOX",
            2_000,
            MAR1 + 86_400_000,
            0,
            "id2@example",
        ),
        (
            "m3",
            "alice@acme.com",
            "Quarterly report",
            "The quarterly report is ready. Revenue grew 12 percent versus last quarter.",
            "ARCHIVE",
            5_500_000,
            APR1,
            0,
            "id3@acme",
        ),
        (
            "m4",
            "carol@example.org",
            "Receipt for lunch",
            "Your receipt: lunch, 14.50 USD. Thank you for dining with us.",
            "INBOX",
            9_000,
            APR1 + 86_400_000,
            1,
            "id4@example",
        ),
        (
            "m5",
            "dave@acme.com",
            "100% off_sale",
            "Literal percent and underscore subject for LIKE escaping tests.",
            "SPAM",
            800,
            APR1,
            1,
            "id5@acme",
        ),
    ];
    for r in rows {
        conn.execute(
            "INSERT INTO messages(id,sender,subject,body,label,size,date_ms,unread,msgid) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8],
        )
        .unwrap();
    }
    conn.execute_batch(
        "INSERT INTO fts_messages(rowid, subject, body) SELECT rowid, subject, body FROM messages;",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO attachments VALUES ('m1','invoice-march.pdf'), ('m3','report_q1.xlsx')",
        [],
    )
    .unwrap();
    (dir, conn)
}

fn ids(conn: &Connection, query: &str) -> Vec<String> {
    let schema = schema();
    let q = schema
        .parse(query)
        .unwrap_or_else(|e| panic!("parse {query:?}: {e}"));
    rows_for(conn, &schema, &q)
}

fn lenient_ids(conn: &Connection, query: &str) -> Vec<String> {
    let schema = schema();
    let q = Query::parse_lenient(query, &schema.syntax());
    rows_for(conn, &schema, &q)
}

fn rows_for(conn: &Connection, schema: &Schema, q: &Query) -> Vec<String> {
    let compiled = schema.compile_at(q, APR1 + 10 * 86_400_000).unwrap();
    let sql = compiled.select("SELECT m.id FROM messages m") + " ORDER BY m.id";
    let mut stmt = conn
        .prepare(&sql)
        .unwrap_or_else(|e| panic!("sql {sql}: {e}"));
    let rows = stmt
        .query_map(params_from_iter(compiled.params.iter()), |r| {
            r.get::<_, String>(0)
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

#[test]
fn free_text_phrase_and_prefix_use_fts5() {
    let (_d, conn) = fixture();
    assert_eq!(ids(&conn, "invoice"), ["m1"]);
    assert_eq!(ids(&conn, "lunch"), ["m2", "m4"]);
    assert_eq!(ids(&conn, "quart"), ["m3"], "bare words are prefix terms");
    assert_eq!(ids(&conn, "\"new ramen place\""), ["m2"]);
    assert!(
        ids(&conn, "\"ramen new\"").is_empty(),
        "phrase order is respected"
    );
    // FTS5 punctuation must not raise a syntax error.
    assert!(ids(&conn, "appeal/x").is_empty());
}

#[test]
fn operators_combine_and_negate() {
    let (_d, conn) = fixture();
    assert_eq!(ids(&conn, "from:acme"), ["m1", "m3", "m5"]);
    assert_eq!(ids(&conn, "from:acme -label:SPAM"), ["m1", "m3"]);
    assert_eq!(ids(&conn, "lunch is:unread"), ["m4"]);
    assert_eq!(ids(&conn, "-from:alice"), ["m2", "m4", "m5"]);
    assert_eq!(
        ids(&conn, "subject:lunch -receipt"),
        ["m2"],
        "negated free text excludes via FTS"
    );
    assert_eq!(ids(&conn, "has:attachment"), ["m1", "m3"]);
    assert_eq!(ids(&conn, "filename:report"), ["m3"]);
    assert_eq!(ids(&conn, "rfc822msgid:<id2@example>"), ["m2"]);
}

#[test]
fn size_and_date_values_bind_as_numbers() {
    let (_d, conn) = fixture();
    assert_eq!(ids(&conn, "larger:1m"), ["m3"]);
    assert_eq!(ids(&conn, "smaller:10k larger:1k"), ["m2", "m4"]);
    assert_eq!(ids(&conn, "after:2024-04-01"), ["m3", "m4", "m5"]);
    assert_eq!(ids(&conn, "before:2024/03/02"), ["m1"]);
    // "now" fixed at 2024-04-11: newer:7d keeps nothing older than Apr 4, newer:15d keeps Apr items and Mar 28+.
    assert!(ids(&conn, "newer:7d").is_empty());
    assert_eq!(ids(&conn, "newer:15d"), ["m3", "m4", "m5"]);
}

#[test]
fn or_groups_over_text_and_operators() {
    let (_d, conn) = fixture();
    assert_eq!(ids(&conn, "invoice OR report"), ["m1", "m3"]);
    assert_eq!(ids(&conn, "{ ramen receipt }"), ["m2", "m4"]);
    // Operator disjunction: the donor flattened this to AND (matching nothing).
    assert_eq!(ids(&conn, "from:bob OR from:carol"), ["m2", "m4"]);
    assert_eq!(ids(&conn, "(from:acme larger:1m)"), ["m3"]);
    assert_eq!(
        ids(&conn, "from:bob OR report"),
        ["m2", "m3"],
        "mixed operator/text disjunction"
    );
}

#[test]
fn like_wildcards_in_values_are_literal() {
    let (_d, conn) = fixture();
    assert_eq!(ids(&conn, "subject:100%"), ["m5"]);
    assert!(
        ids(&conn, "subject:1%0").is_empty(),
        "% in the query must not act as a wildcard"
    );
    assert_eq!(ids(&conn, "subject:off_sale"), ["m5"]);
    assert!(ids(&conn, "subject:off-sale").is_empty());
}

#[test]
fn typed_errors_instead_of_silent_degradation() {
    let schema = schema();
    assert!(
        matches!(schema.parse("bogus:value"), Err(ParseError::UnsupportedOperator { operator, .. }) if operator == "bogus")
    );
    assert!(matches!(
        schema.parse("(from:a"),
        Err(ParseError::UnbalancedGroup { .. })
    ));
    assert!(matches!(schema.parse("()"), Err(ParseError::EmptyGroup)));
    // URLs and bare colons stay free text.
    assert!(schema.parse("https://example.com/x").is_ok());
    let q = schema.parse("larger:abc").unwrap();
    assert!(matches!(
        schema.compile(&q),
        Err(CompileError::BadValue { .. })
    ));
    // Negated quoted phrase keeps its negation (donor lost it).
    let (_d, conn) = fixture();
    assert_eq!(ids(&conn, "lunch -\"new ramen\""), ["m4"]);
}

#[test]
fn fts_snippets_mark_matches_and_split_into_spans() {
    let (_d, conn) = fixture();
    let schema = schema();
    let q = schema.parse("ramen").unwrap();
    let compiled = schema.compile(&q).unwrap();
    let sql = format!(
        "SELECT {} FROM fts_messages WHERE fts_messages MATCH ?1",
        fts_snippet_expr("fts_messages", 8)
    );
    let snippet: String = conn
        .query_row(&sql, [compiled.fts_match.as_deref().unwrap()], |r| r.get(0))
        .unwrap();
    let spans = split_highlights(&snippet);
    let matched: Vec<&str> = spans
        .iter()
        .filter(|s| s.matched)
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(matched, ["ramen"], "snippet {snippet:?}");
    assert!(
        spans.iter().any(|s| !s.matched && !s.text.is_empty()),
        "snippet should preserve unmatched context: {snippet:?}"
    );
}

#[test]
fn empty_query_matches_everything_and_compiles_to_a_noop() {
    let (_d, conn) = fixture();
    assert_eq!(ids(&conn, "   ").len(), 5);
    assert_eq!(lenient_ids(&conn, "   ").len(), 5);
}

#[test]
fn malformed_lenient_query_matches_nothing() {
    let (_d, conn) = fixture();
    for bad in [")", "(from:alice", "from:alice )", "}", "nosuchop:x"] {
        assert_eq!(
            lenient_ids(&conn, bad),
            Vec::<String>::new(),
            "query {bad:?}"
        );
    }
    assert!(Query::parse_lenient(")", &schema().syntax()).matches_nothing());
    assert!(!Query::parse_lenient("   ", &schema().syntax()).matches_nothing());
    assert!(!Query::parse_lenient("", &schema().syntax()).matches_nothing());
}

#[test]
fn strict_parse_still_reports_malformed_queries() {
    let schema = schema();
    assert!(matches!(
        schema.parse(")"),
        Err(ParseError::UnbalancedGroup { .. })
    ));
}
