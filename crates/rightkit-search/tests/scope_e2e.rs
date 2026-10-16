//! Parent/child match scope: threads (parent) over messages (child) in SQLite.

use rightkit_search::{Binding, ChildScope, Schema};
use rusqlite::{params_from_iter, Connection};

fn schema() -> Schema {
    Schema::new()
        .fts("fts_messages", "m.rowid")
        .bind("from", Binding::Like("m.sender"))
        .bind("is", Binding::Flags(&[("unread", "m.unread = 1")]))
        .scope(ChildScope::new("messages m", "m.thread_id = t.id"))
}

fn fixture() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE threads(id TEXT PRIMARY KEY);
         CREATE TABLE messages(thread_id TEXT, sender TEXT, subject TEXT, body TEXT, unread INTEGER);
         CREATE VIRTUAL TABLE fts_messages USING fts5(subject, body, content='messages', content_rowid='rowid');
         INSERT INTO threads VALUES ('t1'),('t2'),('t3');
         -- t1: alice invoice, bob reply (no invoice)
         INSERT INTO messages VALUES ('t1','alice@acme.com','Invoice','invoice due',0);
         INSERT INTO messages VALUES ('t1','bob@x.org','Re: Invoice','thanks',1);
         -- t2: alice lunch, bob invoice
         INSERT INTO messages VALUES ('t2','alice@acme.com','Lunch','ramen',0);
         INSERT INTO messages VALUES ('t2','bob@x.org','Re: Lunch','invoice maybe',0);
         -- t3: only carol
         INSERT INTO messages VALUES ('t3','carol@y.net','Hello','nothing',0);
         INSERT INTO fts_messages(rowid, subject, body) SELECT rowid, subject, body FROM messages;",
    )
    .unwrap();
    conn
}

fn run(conn: &Connection, input: &str) -> Vec<String> {
    let s = schema();
    let c = s.compile(&s.parse(input).unwrap()).unwrap();
    let sql = c.select("SELECT t.id FROM threads t") + " ORDER BY t.id";
    let mut stmt = conn.prepare(&sql).unwrap();
    let rows = stmt
        .query_map(params_from_iter(c.params.iter()), |r| r.get::<_, String>(0))
        .unwrap();
    rows.map(Result::unwrap).collect()
}

#[test]
fn positive_terms_must_cooccur_in_one_message() {
    let conn = fixture();
    // t1 has alice and invoice in the same message; t2 has them in different messages.
    assert_eq!(run(&conn, "from:alice invoice"), vec!["t1"]);
}

#[test]
fn negated_operator_drops_whole_thread_when_any_message_matches() {
    let conn = fixture();
    // bob wrote a message in t1 and t2; both threads drop.
    assert_eq!(run(&conn, "-from:bob"), vec!["t3"]);
}

#[test]
fn negated_free_text_applies_at_the_parent() {
    let conn = fixture();
    assert_eq!(run(&conn, "-invoice"), vec!["t3"]);
}

#[test]
fn positive_and_negated_combine() {
    let conn = fixture();
    // alice somewhere in the thread, but no message from bob anywhere.
    assert_eq!(run(&conn, "from:alice -from:bob"), Vec::<String>::new());
    assert_eq!(run(&conn, "from:alice -from:carol"), vec!["t1", "t2"]);
}

#[test]
fn only_negated_terms_emit_only_not_exists() {
    let s = schema();
    let c = s.compile(&s.parse("-from:bob").unwrap()).unwrap();
    assert!(c.where_sql.starts_with("NOT EXISTS (SELECT 1 FROM messages m WHERE m.thread_id = t.id AND "));
    assert_eq!(c.where_sql.matches("EXISTS").count(), 1);
}

#[test]
fn empty_query_is_unscoped_true() {
    let s = schema();
    let c = s.compile(&s.parse("").unwrap()).unwrap();
    assert_eq!(c.where_sql, "1=1");
}

#[test]
fn unscoped_schema_is_unchanged() {
    let s = Schema::new().bind("from", Binding::Like("m.sender"));
    let c = s.compile(&s.parse("-from:bob").unwrap()).unwrap();
    assert_eq!(c.where_sql, "NOT COALESCE((m.sender LIKE ? ESCAPE '\'), 0)");
}
