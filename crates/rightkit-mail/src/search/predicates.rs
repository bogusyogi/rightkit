//! L11: message-scoped predicate builders extracted out of
//! `search_threads_scoped` as part of the parser decomposition (DISPATCH.md
//! L11 item 3 — that function's repeated NOT EXISTS template was flagged as
//! "the parser's target structure"). `positive_message_predicates` builds the
//! per-message AND-list for the positive EXISTS(...); `thread_level_negation`
//! builds the thread-scoped NOT EXISTS clauses appended after it. Both
//! operate on the same `SearchTerms` the compiler in `query.rs` produces.

use super::query::SearchTerms;
use rightkit_search::sql::{Binding, ChildScope, Compiled, Param, Schema, ShapeSql, ValueKind};
use rightkit_search::terms::{OpTerm, Query};

const LABEL_LINK: &str = "tl.account_id = t.account_id AND tl.thread_id = t.id";
const MSG_LINK: &str = "mn.account_id = t.account_id AND mn.thread_id = t.id";

/// Bound params as the text the search layer binds (`Int` never occurs here:
/// every binding below converts with `Text`/`Eq`/FTS kinds).
fn text_params(c: Compiled) -> (String, Vec<String>) {
    let params = c
        .params
        .into_iter()
        .map(|p| match p {
            Param::Text(t) => t,
            Param::Int(i) => i.to_string(),
        })
        .collect();
    (c.where_sql, params)
}

// L10 recipient match (rightkit-search 0.4.0 `Binding::SqlByShape`, #172): the
// operator value is trimmed and lowercased, then the SQL is picked by shape:
// `@domain` -> `addr_norm LIKE '%@domain%'`; contains `@` -> `addr_norm LIKE
// 'term%'`; bare -> `(addr_norm LIKE 'term%' OR name_norm LIKE 'term%')`.
// `ValueKind::Text` binds the value verbatim and the `%` is concatenated in
// SQL, so `%`, `_` and `\` in the term stay LIKE wildcards exactly as the
// hand-built predicate treated them (the escaping `Prefix`/`Suffix` kinds
// would narrow that). Only the correlated columns differ between the
// per-message EXISTS (`m`) and the thread-level NOT EXISTS (`mn`).
macro_rules! recipient_binding {
    ($kind:literal, $acct:literal, $mid:literal) => {
        Binding::SqlByShape {
            domain: ShapeSql {
                sql: concat!(
                    "EXISTS(SELECT 1 FROM message_recipients mr WHERE mr.account_id = ",
                    $acct,
                    " AND mr.message_id = ",
                    $mid,
                    " AND mr.kind = '",
                    $kind,
                    "' AND mr.addr_norm LIKE ('%' || ? || '%'))"
                ),
                kinds: &[ValueKind::Text],
            },
            address: ShapeSql {
                sql: concat!(
                    "EXISTS(SELECT 1 FROM message_recipients mr WHERE mr.account_id = ",
                    $acct,
                    " AND mr.message_id = ",
                    $mid,
                    " AND mr.kind = '",
                    $kind,
                    "' AND mr.addr_norm LIKE (? || '%'))"
                ),
                kinds: &[ValueKind::Text],
            },
            bare: ShapeSql {
                sql: concat!(
                    "EXISTS(SELECT 1 FROM message_recipients mr WHERE mr.account_id = ",
                    $acct,
                    " AND mr.message_id = ",
                    $mid,
                    " AND mr.kind = '",
                    $kind,
                    "' AND (mr.addr_norm LIKE (? || '%') OR mr.name_norm LIKE (? || '%')))"
                ),
                kinds: &[ValueKind::Text, ValueKind::Text],
            },
            lowercase: true,
        }
    };
}

fn recipient_binding(kind: &str, negated: bool) -> Binding {
    match (kind, negated) {
        ("from", false) => recipient_binding!("from", "m.account_id", "m.id"),
        ("to", false) => recipient_binding!("to", "m.account_id", "m.id"),
        ("cc", false) => recipient_binding!("cc", "m.account_id", "m.id"),
        ("bcc", false) => recipient_binding!("bcc", "m.account_id", "m.id"),
        ("from", true) => recipient_binding!("from", "mn.account_id", "mn.id"),
        ("to", true) => recipient_binding!("to", "mn.account_id", "mn.id"),
        ("cc", true) => recipient_binding!("cc", "mn.account_id", "mn.id"),
        ("bcc", true) => recipient_binding!("bcc", "mn.account_id", "mn.id"),
        _ => unreachable!("recipient kind is one of from/to/cc/bcc"),
    }
}

/// One recipient clause for `kind` and raw `term`: the positive per-message
/// `(EXISTS(...))` over alias `m`, or (negated) the thread-level
/// `NOT EXISTS (SELECT 1 FROM messages mn WHERE <link> AND (EXISTS(...)))`.
/// Compiled one term at a time so bind order follows the caller's order.
fn recipient_clause(kind: &'static str, term: &str, negated: bool) -> (String, Vec<String>) {
    let mut schema = Schema::new().bind(kind, recipient_binding(kind, negated));
    if negated {
        schema = schema.scope(ChildScope::new("messages mn", MSG_LINK));
    }
    let mut q = Query::default();
    q.ops.push(OpTerm {
        op: kind.into(),
        value: term.to_string(),
        negated,
        is_phrase: false,
    });
    // Infallible: `Text` conversion cannot fail, the bind count is fixed by
    // the static fragments, and no OR group or op source is involved.
    text_params(
        schema
            .compile(&q)
            .expect("recipient clause compiles over message_recipients"),
    )
}

/// `NOT EXISTS (SELECT 1 FROM messages mn WHERE <link> AND (subject LIKE ? OR
/// COALESCE(snippet,'') LIKE ? OR mn.rowid IN (... fts_messages MATCH ?)))`
/// for one negated free-text term (rightkit-search 0.4.0 `negated_text_like` +
/// `negated_text_unescaped`, #172): binds `%term%`, `%term%`, `"term"*`. A term
/// with no alphanumeric character has no FTS form and is matched by the two
/// LIKE columns only.
fn negated_text_clause(term: &str) -> (String, Vec<String>) {
    let schema = Schema::new().fts("fts_messages", "mn.rowid").scope(
        ChildScope::new("messages mn", MSG_LINK)
            .negated_text_like(&["mn.subject", "COALESCE(mn.snippet,'')"])
            .negated_text_unescaped(),
    );
    let mut q = Query::default();
    q.negated_text.push(term.to_string());
    text_params(
        schema
            .compile(&q)
            .expect("negated text compiles over messages and fts_messages"),
    )
}

/// `NOT EXISTS (SELECT 1 FROM thread_labels tl WHERE <link> AND (tl.label = ?))`
/// compiled by rightkit-search from a single negated `label:` term.
fn negated_label_clause(label: &str) -> (String, Vec<String>) {
    let schema = Schema::new().bind("label", Binding::Eq("tl.label")).scope(
        ChildScope::new("thread_labels tl", LABEL_LINK).op_source(
            "label",
            "thread_labels tl",
            LABEL_LINK,
        ),
    );
    let mut q = Query::default();
    q.ops.push(OpTerm {
        op: "label".into(),
        value: label.to_string(),
        negated: true,
        is_phrase: false,
    });
    // Infallible by construction: `label` is bound with `Eq` (no value
    // conversion), has an op source, and no OR group is involved.
    text_params(
        schema
            .compile(&q)
            .expect("negated label compiles over thread_labels"),
    )
}

/// Positive per-message operator bindings (correlated on alias `m`), compiled
/// by rightkit-search 0.4.0: `Like` (escaped contains), `Cmp` over `SizeBytes`,
/// `Sql` with `BareId`/`Contains`/`Text`, and `Flags` for `has:attachment`.
fn message_schema() -> Schema {
    Schema::new()
        .bind("subject", Binding::Like("m.subject"))
        .bind(
            "filename",
            Binding::Sql {
                sql: "m.id IN (SELECT message_id FROM attachments WHERE m.account_id = attachments.account_id AND filename LIKE ? ESCAPE '\\')",
                kind: ValueKind::Contains,
            },
        )
        .bind(
            "has",
            Binding::Flags(&[
                ("attachment", "m.has_attachments = 1"),
                ("attachments", "m.has_attachments = 1"),
            ]),
        )
        .bind(
            "larger",
            Binding::Cmp {
                column: "m.size_estimate",
                cmp: ">=",
                kind: ValueKind::SizeBytes,
            },
        )
        .bind(
            "smaller",
            Binding::Cmp {
                column: "m.size_estimate",
                cmp: "<",
                kind: ValueKind::SizeBytes,
            },
        )
        .bind(
            "rfc822msgid",
            Binding::Sql {
                sql: "TRIM(m.rfc822_message_id, '<>') = ?",
                kind: ValueKind::BareId,
            },
        )
        .bind(
            "label",
            Binding::Sql {
                sql: "EXISTS(SELECT 1 FROM thread_labels tlpos WHERE tlpos.account_id = m.account_id AND tlpos.thread_id = m.thread_id AND tlpos.label = ?)",
                kind: ValueKind::Text,
            },
        )
}

/// The message-scoped AND-list for the positive `EXISTS(SELECT 1 FROM
/// messages m WHERE ...)` clause, plus its bound params in bind order. `msg`
/// always starts with the two join conditions the caller's EXISTS needs
/// (`m.account_id = t.account_id`, `m.thread_id = t.id`).
pub fn positive_message_predicates(q: &SearchTerms) -> (Vec<String>, Vec<String>) {
    let mut msg: Vec<String> = vec![
        "m.account_id = t.account_id".into(),
        "m.thread_id = t.id".into(),
    ];
    let mut p: Vec<String> = Vec::new();

    if let Some(fts) = q.fts() {
        msg.push("m.rowid IN (SELECT rowid FROM fts_messages WHERE fts_messages MATCH ?)".into());
        p.push(fts);
    }
    // L10: from:/to:/cc:/bcc: resolve against the normalized, indexed
    // message_recipients table (db/recipients.rs) instead of a leading-
    // wildcard LIKE over the raw header-string columns. Correlated on
    // m.account_id / m.id since these predicates live inside the per-message
    // EXISTS(SELECT 1 FROM messages m WHERE ...) the caller builds.
    if let Some(f) = &q.from {
        let (sql, params) = recipient_clause("from", f, false);
        msg.push(sql);
        p.extend(params);
    }
    if let Some(t) = &q.to {
        let (sql, params) = recipient_clause("to", t, false);
        msg.push(sql);
        p.extend(params);
    }
    if let Some(c) = &q.cc {
        let (sql, params) = recipient_clause("cc", c, false);
        msg.push(sql);
        p.extend(params);
    }
    if let Some(b) = &q.bcc {
        let (sql, params) = recipient_clause("bcc", b, false);
        msg.push(sql);
        p.extend(params);
    }
    // Subject, attachment, size, Message-ID and positive label terms compile
    // through one rightkit-search `Schema` (see `message_schema`), in the
    // order the old hand-built predicates were pushed. NULL `size_estimate`
    // rows (pre-migration, not yet backfilled) never satisfy a bound.
    let mut query = Query::default();
    let mut op = |name: &str, value: String| {
        query.ops.push(OpTerm {
            op: name.into(),
            value,
            negated: false,
            is_phrase: false,
        });
    };
    if let Some(s) = &q.subject {
        op("subject", s.clone());
    }
    if let Some(f) = &q.filename {
        op("filename", f.clone());
    }
    if q.has_attachment {
        op("has", "attachment".into());
    }
    if let Some(min) = q.size_min {
        op("larger", min.to_string());
    }
    if let Some(max) = q.size_max {
        op("smaller", max.to_string());
    }
    // The query-side Message-ID already lost its own angle brackets
    // (query.rs::apply_op); `BareId` strips them again and the column side is
    // TRIMmed, so both stored forms compare equal.
    if let Some(mid) = &q.rfc822_message_id {
        op("rfc822msgid", mid.clone());
    }
    // Positive label: filter messages that carry the label so the per-message
    // EXISTS rolls them up.
    if let Some(label) = q
        .label
        .as_deref()
        .filter(|l| !l.is_empty())
        .filter(|_| !q.label_negated)
    {
        op("label", label.to_string());
    }
    if !query.ops.is_empty() {
        // Infallible: every value is already parsed (sizes are integers),
        // every binding is static, and no OR group or op source is involved.
        let (sql, params) = text_params(
            message_schema()
                .compile(&query)
                .expect("positive message predicates compile over messages"),
        );
        msg.push(sql);
        p.extend(params);
    }

    (msg, p)
}

/// The thread-scoped `AND NOT EXISTS(...)` clauses for every negated
/// operator/text term, appended to `where_sql` after the positive EXISTS.
/// Gmail negation semantics: a thread is dropped if ANY message in it
/// matches the negative clause (thread-level NOT EXISTS), never per-message.
/// Returns the accumulated SQL fragment (starting with a leading space) and
/// its bound params in bind order.
pub fn thread_level_negation(q: &SearchTerms) -> (String, Vec<String>) {
    let mut sql = String::new();
    let mut p: Vec<String> = Vec::new();

    for (kind, terms) in [
        ("from", &q.negated_from),
        ("to", &q.negated_to),
        ("cc", &q.cc_negated),
        ("bcc", &q.bcc_negated),
    ] {
        for term in terms {
            let (clause, params) = recipient_clause(kind, term, true);
            sql.push_str(" AND ");
            sql.push_str(&clause);
            p.extend(params);
        }
    }
    for nt in &q.negated_text {
        let (clause, params) = negated_text_clause(nt);
        sql.push_str(" AND ");
        sql.push_str(&clause);
        p.extend(params);
    }
    // Negated label: thread-level NOT EXISTS so a single matching message
    // drops the whole thread.
    if let Some(label) = q
        .label
        .as_deref()
        .filter(|l| !l.is_empty())
        .filter(|_| q.label_negated)
    {
        // rightkit-search 0.4.0 `ChildScope::op_source`: semantically identical
        // predicate (thread_labels correlated on account_id + thread_id,
        // `label = ?`, one verbatim bind), compiled instead of hand-built.
        let (clause, params) = negated_label_clause(label);
        sql.push_str(" AND ");
        sql.push_str(&clause);
        p.extend(params);
    }

    (sql, p)
}
