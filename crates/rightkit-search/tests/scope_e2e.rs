//! Parent/child match scope: threads (parent) over messages (child) in SQLite.

use rightkit_search::{Binding, ChildScope, CompileError, Schema, ValueKind};
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
    assert_eq!(c.where_sql, "NOT COALESCE((m.sender LIKE ? ESCAPE '\\'), 0)");
}

// ---- Multi-bind, LIKE+FTS negated text, per-operator child sources (#166) ----

const RECIPIENT_SQL: &str = "EXISTS (SELECT 1 FROM message_recipients r WHERE r.message_id = m.id AND (r.addr LIKE ? ESCAPE '\\' OR r.addr LIKE ? ESCAPE '\\'))";

fn mail_schema(with_like: bool) -> Schema {
    let mut scope = ChildScope::new("messages m", "m.thread_id = t.id")
        .op_source("label", "thread_labels l", "l.thread_id = t.id");
    if with_like {
        scope = scope.negated_text_like(&["m.subject", "m.snippet"]);
    }
    Schema::new()
        .fts("fts_messages", "m.rowid")
        .bind("from", Binding::Like("m.sender"))
        .bind(
            "to",
            Binding::SqlMulti {
                sql: RECIPIENT_SQL,
                kinds: &[ValueKind::Prefix, ValueKind::Suffix],
            },
        )
        .bind("label", Binding::Eq("l.label"))
        .scope(scope)
}

fn mail_fixture() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE threads(id TEXT PRIMARY KEY);
         CREATE TABLE messages(id INTEGER PRIMARY KEY, thread_id TEXT, sender TEXT, subject TEXT, snippet TEXT, body TEXT);
         CREATE TABLE message_recipients(message_id INTEGER, addr TEXT);
         CREATE TABLE thread_labels(thread_id TEXT, label TEXT);
         CREATE VIRTUAL TABLE fts_messages USING fts5(subject, body, content='messages', content_rowid='id');
         INSERT INTO threads VALUES ('t1'),('t2'),('t3'),('t4'),('t5');
         -- t1: 'budget' in subject (also in FTS)
         INSERT INTO messages VALUES (1,'t1','a@x.org','Budget','quarterly','sheet');
         -- t2: 'budget' only in snippet (not indexed by FTS)
         INSERT INTO messages VALUES (2,'t2','a@x.org','Lunch','budget talk','ramen');
         -- t3: 'budget' in subject
         INSERT INTO messages VALUES (3,'t3','b@x.org','Budget review','x','y');
         -- t4: 'budget' only in body (FTS)
         INSERT INTO messages VALUES (4,'t4','b@x.org','Hello','hi','budget');
         -- t5: no hit
         INSERT INTO messages VALUES (5,'t5','c@x.org','Plain','hi','nothing');
         INSERT INTO message_recipients VALUES (1,'dana@corp.com'),(2,'erin@other.org'),(3,'frank@corp.com'),(4,'gina@sub.corp.com'),(5,'hal@corp.com');
         INSERT INTO thread_labels VALUES ('t1','work'),('t5','work'),('t5','spam'),('t2','spam');
         INSERT INTO fts_messages(rowid, subject, body) SELECT id, subject, body FROM messages;",
    )
    .unwrap();
    conn
}

fn run_with(conn: &Connection, s: &Schema, input: &str) -> Vec<String> {
    let c = s.compile(&s.parse(input).unwrap()).unwrap();
    let sql = c.select("SELECT t.id FROM threads t") + " ORDER BY t.id";
    let mut stmt = conn.prepare(&sql).unwrap();
    let rows = stmt
        .query_map(params_from_iter(c.params.iter()), |r| r.get::<_, String>(0))
        .unwrap();
    rows.map(Result::unwrap).collect()
}

#[test]
fn negated_text_excludes_on_subject_snippet_or_fts() {
    let conn = mail_fixture();
    // subject (t1, t3), snippet (t2) and FTS body (t4) all drop; only t5 stays.
    assert_eq!(run_with(&conn, &mail_schema(true), "-budget"), vec!["t5"]);
}

#[test]
fn negated_text_without_like_columns_is_fts_only() {
    let conn = mail_fixture();
    // The snippet is not indexed, so t2 survives when no LIKE columns are configured.
    assert_eq!(run_with(&conn, &mail_schema(false), "-budget"), vec!["t2", "t5"]);
}

#[test]
fn negated_text_binds_three_params_per_term() {
    let s = mail_schema(true);
    let c = s.compile(&s.parse("-budget").unwrap()).unwrap();
    assert_eq!(c.params.len(), 3);
    assert_eq!(c.where_sql.matches('?').count(), 3);
}

#[test]
fn multi_bind_sql_matches_prefix_and_domain_suffix() {
    let conn = mail_fixture();
    let s = mail_schema(true);
    // Prefix bind.
    assert_eq!(run_with(&conn, &s, "to:dana"), vec!["t1"]);
    // Suffix bind (`@corp.com` does not match gina@sub.corp.com).
    assert_eq!(run_with(&conn, &s, "to:@corp.com"), vec!["t1", "t3", "t5"]);
}

#[test]
fn multi_bind_negated_drops_thread() {
    let conn = mail_fixture();
    assert_eq!(
        run_with(&conn, &mail_schema(true), "-to:@corp.com"),
        vec!["t2", "t4"]
    );
}

#[test]
fn multi_bind_count_mismatch_is_an_error() {
    let s = Schema::new()
        .bind(
            "to",
            Binding::SqlMulti {
                sql: "m.a LIKE ? OR m.b LIKE ?",
                kinds: &[ValueKind::Prefix],
            },
        )
        .scope(ChildScope::new("messages m", "m.thread_id = t.id"));
    let err = s.compile(&s.parse("to:x").unwrap()).unwrap_err();
    assert!(matches!(
        err,
        CompileError::BindCount { expected: 1, found: 2, .. }
    ));
}

#[test]
fn negation_over_a_different_child_table() {
    let conn = mail_fixture();
    let s = mail_schema(true);
    // -label:spam goes through thread_labels, not messages: t2 and t5 drop.
    assert_eq!(run_with(&conn, &s, "-label:spam"), vec!["t1", "t3", "t4"]);
    let c = s.compile(&s.parse("-label:spam").unwrap()).unwrap();
    assert!(c
        .where_sql
        .starts_with("NOT EXISTS (SELECT 1 FROM thread_labels l WHERE l.thread_id = t.id AND "));
    assert!(!c.where_sql.contains("messages m"));
}

#[test]
fn positive_op_with_own_source_and_mixed_negations() {
    let conn = mail_fixture();
    let s = mail_schema(true);
    // work-labelled threads (t1, t5), minus a budget mention anywhere, minus spam.
    assert_eq!(run_with(&conn, &s, "label:work"), vec!["t1", "t5"]);
    assert_eq!(run_with(&conn, &s, "label:work -budget"), vec!["t5"]);
    assert_eq!(run_with(&conn, &s, "label:work -label:spam"), vec!["t1"]);
    // Several negation sources in one query: messages (from, text) and thread_labels.
    assert_eq!(
        run_with(&conn, &s, "-from:b@x.org -label:spam -nothing"),
        vec!["t1"]
    );
}

#[test]
fn op_source_in_or_group_is_rejected() {
    let s = mail_schema(true);
    let err = s
        .compile(&s.parse("label:work OR from:a").unwrap())
        .unwrap_err();
    assert_eq!(err, CompileError::OpSourceInOrGroup("label".to_string()));
}

#[test]
fn injection_shaped_values_stay_bound_parameters() {
    let conn = mail_fixture();
    let s = mail_schema(true);
    for input in [
        "-x';DROP--",
        "to:x';DROP--",
        "-to:x';DROP--",
        "-label:x';DROP--",
        "label:x';DROP--",
        "-100%",
        "to:%",
        "-_",
    ] {
        let c = s.compile(&s.parse(input).unwrap()).unwrap();
        assert!(!c.where_sql.contains("DROP"), "{input}: value leaked into SQL");
        let rows = run_with(&conn, &s, input);
        // The tables survive.
        let n: i64 = conn
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 5, "{input}");
        // Negations of values nobody contains, and escaped wildcards, drop nothing.
        if input.starts_with('-') && !input.starts_with("-to:") && !input.starts_with("-label:") {
            assert_eq!(rows.len(), 5, "{input}");
        }
    }
}
