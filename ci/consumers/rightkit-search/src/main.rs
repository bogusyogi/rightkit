// rightkit-search consumed by the exact registry pin in Cargo.toml, outside the workspace, with every
// feature on: the default "sqlite" (Param binds as rusqlite::ToSql) and "filename-index". That is the
// union of what the apps select: most turn "sqlite" off, two keep it, one adds "filename-index". The
// consumer adds rusqlite with its own "bundled" feature so the compiled SQL runs on a real SQLite.
//
// Proves the public query surface a product builds its search on, with explicit clocks:
//   * lexer: tokenize splits words, operator:value, quoted phrases (with an operator prefix),
//     negation, the structural ( ) { } and a standalone case-insensitive OR.
//   * ast and terms: Syntax lowercases its vocabulary; parse and Query::parse build operator terms,
//     free text, phrases, OR groups, operator OR groups and negations; an unknown operator, an empty
//     group and unbalanced grouping are typed errors; a URL is free text; parse_lenient turns
//     malformed input into a query that matches nothing; literal treats the whole input as one
//     phrase; filters_only keeps only operator terms; fts and fts_negated build the FTS5 expressions.
//   * values: parse_date_ms, parse_relative_ms (explicit now) and parse_size_bytes.
//   * sql: Schema::compile_at turns a Query into a parameterised WHERE body for every Binding
//     (Like, Eq, Cmp with each ValueKind, Sql, SqlMulti, SqlByShape, Flags), negation, OR groups,
//     free text through an FTS table, a parent/child ChildScope with a separate operator source, and
//     each CompileError.
//   * snippet, fold, matcher: the FTS5 snippet expression, highlight splitting and excerpts; Unicode
//     default case folding (sharp s, sigma, ligatures, Turkic option) and the fold-version check;
//     TextMatcher over eager and lazy haystacks (AND, phrase, OR group, negation, literal % and _,
//     MixedOrGroup refusal) and its highlighted snippets with the cut flags.
//   * page and scan: Keyset paging SQL and parameters (descending and ascending, range bound,
//     cursor expressions, each CursorError), the opaque Cursor text form and its decode failures;
//     scan_page budgets, limits, resume cursors, short sources and error propagation; and
//     read_text_prefix cutting at a character boundary.
//   * filename (feature filename-index): FilenameIndex insert, search by folded name, kind,
//     extension and size, deterministic path order, pagination and has_more, every validation error,
//     de-duplication and remove_tree.
//   * sqlite (feature sqlite): Param binds as Text and Int; compiled predicates run on an in-memory
//     SQLite with an FTS5 external-content table and return the expected rows for free text, phrases,
//     prefixes, negation, escaped LIKE wildcards, every Binding, OR groups, mixed groups and a
//     parent/child scope; the FTS5 snippet expression runs and splits into highlight spans; keyset
//     paging walks every row once in order across encoded and decoded cursors, with and without the
//     range bound.
// Public API only; every public module is imported by path, so a removed item fails to compile.
// Does not prove: now_ms (it reads the system clock) or Schema::compile (it calls now_ms; compile_at
// is proven instead), or SQLite behaviour beyond the statements the crate emits.
// State: one small file under a per-process directory in the OS temp directory, removed at the end,
// and an in-memory SQLite database. No network, no process spawn, no wall clock, no real user data.
use rightkit_search::ast::{self, Leaf, Node, ParseError, Syntax};
use rightkit_search::filename::{
    Entry, Error as FilenameError, FilenameIndex, Kind, Page, Request, MAX_EXTENSION_LENGTH,
    MAX_PAGE_LIMIT, MAX_PAGE_OFFSET, MAX_QUERY_LENGTH,
};
use rightkit_search::fold::folded_len;
use rightkit_search::lexer::{tokenize, Token};
use rightkit_search::page::{Cursor, CursorError, Keyset};
use rightkit_search::scan::{read_text_prefix, scan_page, ScanBudget};
use rightkit_search::snippet::{
    contains_any_term, excerpt_around, fts_snippet_expr, split_highlights, Span, MATCH_END,
    MATCH_START,
};
use rightkit_search::values::{parse_date_ms, parse_relative_ms, parse_size_bytes};
use rightkit_search::{
    check_fold_version, fold, fold_with, Binding, ChildScope, CompileError, FoldOptions,
    MatchError, OpTerm, Param, Query, Schema, ShapeSql, TextMatcher, ValueKind, FOLD_VERSION,
};
use rusqlite::{params_from_iter, Connection};
use std::cell::Cell;
use std::convert::Infallible;
use std::path::PathBuf;

const NOW: i64 = 1_800_000_000_000;
const DAY: i64 = 86_400_000;

fn text(s: &str) -> Param {
    Param::Text(s.to_owned())
}

fn word(s: &str) -> Token {
    Token::Word(s.to_owned())
}

fn leaf(op: Option<&str>, value: &str, negated: bool, is_phrase: bool) -> Leaf {
    Leaf {
        op: op.map(str::to_owned),
        value: value.to_owned(),
        negated,
        is_phrase,
    }
}

fn syntax() -> Syntax {
    Syntax::new(["From", "label", "subject"])
}

fn lexer() {
    assert_eq!(tokenize(""), vec![]);
    assert_eq!(tokenize("   \t\n"), vec![]);
    assert_eq!(
        tokenize("from:alice -label:x hello"),
        vec![word("from:alice"), word("-label:x"), word("hello")]
    );
    assert_eq!(
        tokenize("subject:\"big deal\""),
        vec![Token::Phrase {
            op_prefix: Some("subject:".to_owned()),
            body: "big deal".to_owned()
        }]
    );
    assert_eq!(
        tokenize("\"alpha beta\""),
        vec![Token::Phrase {
            op_prefix: None,
            body: "alpha beta".to_owned()
        }]
    );
    assert_eq!(
        tokenize("-\"alpha\""),
        vec![Token::Phrase {
            op_prefix: Some("-".to_owned()),
            body: "alpha".to_owned()
        }]
    );
    assert_eq!(
        tokenize("a OR b or c"),
        vec![word("a"), Token::Or, word("b"), Token::Or, word("c")]
    );
    assert_eq!(
        tokenize("ORACLE"),
        vec![word("ORACLE")],
        "OR is only a standalone word"
    );
    assert_eq!(
        tokenize("(a){b}"),
        vec![
            Token::LParen,
            word("a"),
            Token::RParen,
            Token::LBrace,
            word("b"),
            Token::RBrace
        ]
    );
    assert_eq!(
        tokenize("\"unterminated phrase"),
        vec![Token::Phrase {
            op_prefix: None,
            body: "unterminated phrase".to_owned()
        }]
    );
}

fn parser() {
    let syntax = syntax();
    assert!(syntax.knows("from"), "the vocabulary is lowercased");
    assert!(!syntax.knows("From"));
    assert!(!syntax.knows("to"));
    assert_eq!(syntax.operators(), ["from", "label", "subject"]);

    assert_eq!(
        ast::parse("FROM:alice hello", &syntax).unwrap(),
        vec![
            Node::Leaf(leaf(Some("from"), "alice", false, false)),
            Node::Leaf(leaf(None, "hello", false, false))
        ]
    );
    assert_eq!(
        ast::parse("-label:x", &syntax).unwrap(),
        vec![Node::Leaf(leaf(Some("label"), "x", true, false))]
    );
    assert_eq!(
        ast::parse("subject:\"big deal\" -subject:\"no\"", &syntax).unwrap(),
        vec![
            Node::Leaf(leaf(Some("subject"), "big deal", false, true)),
            Node::Leaf(leaf(Some("subject"), "no", true, true))
        ]
    );
    assert_eq!(
        ast::parse("-\"alpha beta\"", &syntax).unwrap(),
        vec![Node::Leaf(leaf(None, "alpha beta", true, true))]
    );
    assert_eq!(
        ast::parse("a OR b", &syntax).unwrap(),
        vec![Node::Or(vec![
            leaf(None, "a", false, false),
            leaf(None, "b", false, false)
        ])]
    );
    assert_eq!(
        ast::parse("{a b c}", &syntax).unwrap(),
        vec![Node::Or(vec![
            leaf(None, "a", false, false),
            leaf(None, "b", false, false),
            leaf(None, "c", false, false)
        ])]
    );
    assert_eq!(
        ast::parse("(a b) c", &syntax).unwrap(),
        vec![
            Node::Leaf(leaf(None, "a", false, false)),
            Node::Leaf(leaf(None, "b", false, false)),
            Node::Leaf(leaf(None, "c", false, false))
        ],
        "a group is spliced into the parent AND list"
    );
    assert_eq!(
        ast::parse("sub*ject", &syntax).unwrap(),
        vec![Node::Leaf(leaf(None, "subject", false, false))],
        "wildcard characters are not search syntax"
    );
    assert_eq!(
        ast::parse("https://example.test/x", &syntax).unwrap(),
        vec![Node::Leaf(leaf(
            None,
            "https://example.test/x",
            false,
            false
        ))],
        "a URL is free text, not an operator"
    );
    assert_eq!(ast::parse("", &syntax).unwrap(), vec![]);

    let unknown = ast::parse("unknown:thing", &syntax).unwrap_err();
    assert_eq!(
        unknown,
        ParseError::UnsupportedOperator {
            operator: "unknown".to_owned(),
            raw: "unknown:thing".to_owned()
        }
    );
    assert_eq!(
        unknown.to_string(),
        "unsupported search operator \"unknown:\" in \"unknown:thing\""
    );
    assert!(matches!(
        ast::parse("unknown:\"quoted\"", &syntax),
        Err(ParseError::UnsupportedOperator { .. })
    ));
    assert_eq!(ast::parse("()", &syntax), Err(ParseError::EmptyGroup));
    assert_eq!(ParseError::EmptyGroup.to_string(), "empty group");
    assert!(matches!(
        ast::parse("(a", &syntax),
        Err(ParseError::UnbalancedGroup { .. })
    ));
    assert!(matches!(
        ast::parse("a)", &syntax),
        Err(ParseError::UnbalancedGroup { .. })
    ));
    assert!(matches!(
        ast::parse("{a", &syntax),
        Err(ParseError::UnbalancedGroup { .. })
    ));
    let as_error: &dyn std::error::Error = &ParseError::EmptyGroup;
    assert!(!as_error.to_string().is_empty());
}

fn queries() {
    let syntax = syntax();
    let input = "from:alice invoice \"exact words\" -spam (alpha OR beta) label:a OR label:b";
    let q = Query::parse(input, &syntax).unwrap();
    assert_eq!(
        Query::from_nodes(ast::parse(input, &syntax).unwrap()),
        q,
        "Query::parse is ast::parse followed by Query::from_nodes"
    );
    assert_eq!(
        q.ops,
        vec![OpTerm {
            op: "from".to_owned(),
            value: "alice".to_owned(),
            negated: false,
            is_phrase: false
        }]
    );
    assert_eq!(q.text, ["invoice"]);
    assert_eq!(q.phrases, ["exact words"]);
    assert_eq!(q.negated_text, ["spam"]);
    assert_eq!(
        q.or_groups,
        vec![vec!["alpha".to_owned(), "beta".to_owned()]]
    );
    assert_eq!(q.op_or_groups.len(), 1);
    assert_eq!(
        q.op_or_groups[0]
            .iter()
            .map(|t| t.value.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert!(q.op_or_groups[0].iter().all(|t| t.op == "label"));
    assert!(!q.is_empty());
    assert!(!q.matches_nothing());
    assert_eq!(q.op_values("from").count(), 1);
    assert_eq!(
        q.op_values("label").count(),
        0,
        "OR-group terms are not in ops"
    );
    assert_eq!(
        q.fts().as_deref(),
        Some("\"invoice\"* \"exact words\" (\"alpha\"* OR \"beta\"*)")
    );
    assert_eq!(q.fts_negated(), ["\"spam\"*"]);
    assert_eq!(
        q.positive_text_terms(),
        ["invoice", "exact words", "alpha", "beta"]
    );

    assert!(Query::default().is_empty());
    assert!(Query::parse("", &syntax).unwrap().is_empty());
    assert_eq!(Query::default().fts(), None);
    assert!(Query::parse("   ", &syntax).unwrap().is_empty());

    // Lenient parsing: malformed input matches nothing, blank input still matches everything.
    let lenient = Query::parse_lenient("(", &syntax);
    assert!(lenient.matches_nothing());
    assert!(
        !lenient.is_empty(),
        "a malformed query restricts to zero rows"
    );
    assert!(Query::parse_lenient("unknown:x", &syntax).matches_nothing());
    assert!(!Query::parse_lenient("", &syntax).matches_nothing());
    assert!(Query::parse_lenient("", &syntax).is_empty());
    assert!(!Query::parse_lenient("from:a", &syntax).matches_nothing());
    assert!(
        lenient.filters_only().matches_nothing(),
        "a malformed query stays malformed"
    );

    // Literal: the whole trimmed input is one phrase; nothing is interpreted.
    let literal = Query::literal("  from:alice OR \"x\"  ");
    assert_eq!(literal.phrases, ["from:alice OR \"x\""]);
    assert!(literal.ops.is_empty() && literal.text.is_empty());
    assert!(Query::literal("   ").is_empty());

    // filters_only keeps operator terms and operator-only OR groups; free text is dropped.
    let filters = q.filters_only();
    assert_eq!(filters.ops, q.ops);
    assert_eq!(filters.op_or_groups, q.op_or_groups);
    assert!(filters.text.is_empty() && filters.phrases.is_empty());
    assert!(filters.or_groups.is_empty() && filters.negated_text.is_empty());
    let mixed = Query::parse("from:a OR bare", &syntax).unwrap();
    assert_eq!(mixed.op_or_groups.len(), 1);
    assert_eq!(
        mixed.op_or_groups[0][1].op, "",
        "free text in a mixed group has an empty operator"
    );
    assert!(
        mixed.filters_only().op_or_groups.is_empty(),
        "a mixed group has no filter meaning"
    );
    assert_eq!(
        TextMatcher::new(&mixed).unwrap_err(),
        MatchError::MixedOrGroup
    );

    // FTS expressions skip terms with no alphanumeric character.
    let punctuation = Query::parse("--- ok", &syntax).unwrap();
    assert_eq!(punctuation.fts().as_deref(), Some("\"ok\"*"));
}

fn values() {
    assert_eq!(parse_date_ms("1970-01-02"), Some(DAY));
    assert_eq!(parse_date_ms("2024-02-29"), Some(1_709_164_800_000));
    assert_eq!(parse_date_ms("2024/02/29"), Some(1_709_164_800_000));
    assert_eq!(parse_date_ms(" 2024-02-29 "), Some(1_709_164_800_000));
    assert_eq!(parse_date_ms("2024-2-9"), Some(1_707_436_800_000));
    assert_eq!(parse_date_ms("1969-12-31"), Some(-DAY));
    for bad in [
        "2023-02-29",
        "2024-13-01",
        "2024-00-10",
        "2024-01-32",
        "24-01-01",
        "2024-01",
        "2024-01-01-01",
        "",
        "today",
    ] {
        assert_eq!(parse_date_ms(bad), None, "must refuse {bad:?}");
    }

    assert_eq!(parse_relative_ms("7d", NOW), Some(NOW - 7 * DAY));
    assert_eq!(
        parse_relative_ms("2m", NOW),
        Some(NOW - 60 * DAY),
        "a month is 30 days"
    );
    assert_eq!(
        parse_relative_ms("1y", NOW),
        Some(NOW - 365 * DAY),
        "a year is 365 days"
    );
    assert_eq!(parse_relative_ms("0d", NOW), Some(NOW));
    for bad in ["", "d", "7", "7x", "ad", "7 d"] {
        assert_eq!(parse_relative_ms(bad, NOW), None, "must refuse {bad:?}");
    }

    assert_eq!(parse_size_bytes("512"), Some(512));
    assert_eq!(parse_size_bytes("5k"), Some(5_000));
    assert_eq!(parse_size_bytes("2M"), Some(2_000_000));
    assert_eq!(parse_size_bytes("1g"), Some(1_000_000_000));
    assert_eq!(parse_size_bytes("1.5k"), Some(1_500));
    assert_eq!(parse_size_bytes(" 3k "), Some(3_000));
    for bad in ["", "k", "abc", "-1k", "1kb", "inf", "NaN"] {
        assert_eq!(parse_size_bytes(bad), None, "must refuse {bad:?}");
    }
}

fn mail_schema() -> Schema {
    Schema::new()
        .fts("messages_fts", "m.rowid")
        .bind("from", Binding::Like("m.sender"))
        .bind("label", Binding::Eq("m.label"))
        .bind(
            "size",
            Binding::Cmp {
                column: "m.size",
                cmp: ">",
                kind: ValueKind::SizeBytes,
            },
        )
        .bind(
            "after",
            Binding::Cmp {
                column: "m.received_at",
                cmp: ">=",
                kind: ValueKind::DateMs,
            },
        )
        .bind(
            "newer",
            Binding::Cmp {
                column: "m.received_at",
                cmp: ">",
                kind: ValueKind::RelativeMs,
            },
        )
        .bind(
            "count",
            Binding::Cmp {
                column: "m.parts",
                cmp: "=",
                kind: ValueKind::Int,
            },
        )
        .bind(
            "is",
            Binding::Flags(&[("unread", "m.unread = 1"), ("starred", "m.starred = 1")]),
        )
        .bind(
            "id",
            Binding::Sql {
                sql: "m.msgid = ?",
                kind: ValueKind::BareId,
            },
        )
        .bind(
            "to",
            Binding::SqlByShape {
                domain: ShapeSql {
                    sql: "m.rcpt LIKE ? ESCAPE '\\'",
                    kinds: &[ValueKind::Suffix],
                },
                address: ShapeSql {
                    sql: "m.rcpt = ?",
                    kinds: &[ValueKind::Text],
                },
                bare: ShapeSql {
                    sql: "m.rcpt LIKE ? ESCAPE '\\'",
                    kinds: &[ValueKind::Prefix],
                },
                lowercase: true,
            },
        )
        .bind(
            "cc",
            Binding::SqlMulti {
                sql: "(m.cc LIKE ? ESCAPE '\\' OR m.cc LIKE ? ESCAPE '\\')",
                kinds: &[ValueKind::Prefix, ValueKind::Suffix],
            },
        )
}

fn compile(schema: &Schema, input: &str) -> Result<rightkit_search::Compiled, CompileError> {
    schema.compile_at(&schema.parse(input).expect("fixture query parses"), NOW)
}

const FTS_IN: &str = "m.rowid IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)";
const FTS_NOT_IN: &str =
    "m.rowid NOT IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)";
const SENDER_LIKE: &str = "m.sender LIKE ? ESCAPE '\\'";

fn compiler() {
    let schema = mail_schema();
    assert!(schema.syntax().knows("from") && schema.syntax().knows("cc"));
    assert_eq!(schema.syntax().operators().len(), 10);

    // Free text and a LIKE operator: wildcards in the value are escaped.
    let c = compile(&schema, "from:al_ice% invoice").unwrap();
    assert_eq!(c.where_sql, format!("{FTS_IN} AND ({SENDER_LIKE})"));
    assert_eq!(c.params, vec![text("\"invoice\"*"), text("%al\\_ice\\%%")]);
    assert_eq!(c.fts_match.as_deref(), Some("\"invoice\"*"));
    assert_eq!(
        c.select("SELECT m.id FROM messages m"),
        format!("SELECT m.id FROM messages m WHERE {}", c.where_sql)
    );

    // Empty and malformed queries.
    let empty = compile(&schema, "").unwrap();
    assert_eq!(empty.where_sql, "1=1");
    assert!(empty.params.is_empty() && empty.fts_match.is_none());
    let nothing = schema
        .compile_at(&Query::parse_lenient("(", &schema.syntax()), NOW)
        .unwrap();
    assert_eq!(nothing.where_sql, "0=1");
    assert!(nothing.params.is_empty());

    // Equality, comparison kinds, flags, ids and shape-dependent bindings.
    let c = compile(&schema, "label:work").unwrap();
    assert_eq!(
        (c.where_sql.as_str(), c.params.as_slice()),
        ("(m.label = ?)", [text("work")].as_slice())
    );
    let c = compile(&schema, "size:5k").unwrap();
    assert_eq!(
        (c.where_sql.as_str(), c.params.as_slice()),
        ("(m.size > ?)", [Param::Int(5_000)].as_slice())
    );
    let c = compile(&schema, "after:2024-02-29").unwrap();
    assert_eq!(c.where_sql, "(m.received_at >= ?)");
    assert_eq!(c.params, vec![Param::Int(1_709_164_800_000)]);
    let c = compile(&schema, "newer:7d").unwrap();
    assert_eq!(
        c.params,
        vec![Param::Int(NOW - 7 * DAY)],
        "relative dates use the explicit now"
    );
    let c = compile(&schema, "count:3").unwrap();
    assert_eq!(c.where_sql, "(m.parts = ?)");
    assert_eq!(c.params, vec![Param::Int(3)]);
    let c = compile(&schema, "is:Unread").unwrap();
    assert_eq!(c.where_sql, "(m.unread = 1)");
    assert!(c.params.is_empty());
    let c = compile(&schema, "id:<abc@mail.test>").unwrap();
    assert_eq!(c.where_sql, "(m.msgid = ?)");
    assert_eq!(
        c.params,
        vec![text("abc@mail.test")],
        "angle brackets are stripped"
    );
    let c = compile(&schema, "to:@Corp.com").unwrap();
    assert_eq!(
        c.params,
        vec![text("%@corp.com")],
        "a domain matches as a suffix"
    );
    let c = compile(&schema, "to:Bob@Corp.com").unwrap();
    assert_eq!(c.where_sql, "(m.rcpt = ?)");
    assert_eq!(
        c.params,
        vec![text("bob@corp.com")],
        "an address matches exactly"
    );
    let c = compile(&schema, "to:Bob").unwrap();
    assert_eq!(
        c.params,
        vec![text("bob%")],
        "a bare name matches as a prefix"
    );
    let c = compile(&schema, "cc:ann").unwrap();
    assert_eq!(
        c.where_sql,
        "((m.cc LIKE ? ESCAPE '\\' OR m.cc LIKE ? ESCAPE '\\'))"
    );
    assert_eq!(c.params, vec![text("ann%"), text("%ann")]);

    // Negation is NULL-safe; negated free text is NOT IN over the FTS table.
    let c = compile(&schema, "-from:bob").unwrap();
    assert_eq!(c.where_sql, format!("NOT COALESCE(({SENDER_LIKE}), 0)"));
    assert_eq!(c.params, vec![text("%bob%")]);
    let c = compile(&schema, "-spam").unwrap();
    assert_eq!(c.where_sql, FTS_NOT_IN);
    assert_eq!(c.params, vec![text("\"spam\"*")]);
    assert!(
        c.fts_match.is_none(),
        "negated text is not a positive match expression"
    );

    // OR groups over operators, and a mixed group that includes free text.
    let c = compile(&schema, "from:a OR from:b").unwrap();
    assert_eq!(c.where_sql, format!("(({SENDER_LIKE}) OR ({SENDER_LIKE}))"));
    assert_eq!(c.params, vec![text("%a%"), text("%b%")]);
    let c = compile(&schema, "from:a OR hello").unwrap();
    assert_eq!(c.where_sql, format!("(({SENDER_LIKE}) OR {FTS_IN})"));
    assert_eq!(c.params, vec![text("%a%"), text("\"hello\"*")]);

    // Compile errors.
    assert_eq!(
        compile(&schema, "size:huge").unwrap_err(),
        CompileError::BadValue {
            op: "size".to_owned(),
            value: "huge".to_owned()
        }
    );
    assert!(matches!(
        compile(&schema, "is:bogus"),
        Err(CompileError::BadValue { op, .. }) if op == "is"
    ));
    assert!(matches!(
        compile(&schema, "after:2024-13-45"),
        Err(CompileError::BadValue { .. })
    ));
    let no_fts = Schema::new().bind("from", Binding::Like("m.sender"));
    assert_eq!(
        compile(&no_fts, "hello").unwrap_err(),
        CompileError::NoFtsTable
    );
    assert_eq!(
        compile(&no_fts, "-hello").unwrap_err(),
        CompileError::NoFtsTable
    );
    assert!(
        compile(&no_fts, "from:x").is_ok(),
        "operators alone need no FTS table"
    );
    assert_eq!(
        Schema::new()
            .compile_at(
                &Query::parse("from:x", &Syntax::new(["from"])).unwrap(),
                NOW
            )
            .unwrap_err(),
        CompileError::UnboundOperator("from".to_owned())
    );
    let miscounted = Schema::new().bind(
        "x",
        Binding::SqlMulti {
            sql: "a = ?",
            kinds: &[ValueKind::Text, ValueKind::Text],
        },
    );
    assert_eq!(
        compile(&miscounted, "x:1").unwrap_err(),
        CompileError::BindCount {
            op: "x".to_owned(),
            expected: 2,
            found: 1
        }
    );
    for error in [
        CompileError::NoFtsTable,
        CompileError::UnboundOperator("x".to_owned()),
        CompileError::OpSourceInOrGroup("x".to_owned()),
    ] {
        let as_error: &dyn std::error::Error = &error;
        assert!(!as_error.to_string().is_empty());
    }
    assert_eq!(
        CompileError::BadValue {
            op: "size".to_owned(),
            value: "huge".to_owned()
        }
        .to_string(),
        "invalid value \"huge\" for \"size:\""
    );
}

fn child_scopes() {
    let scope = ChildScope::new("messages m", "m.thread_id = t.id");
    assert_eq!(
        (scope.from, scope.link),
        ("messages m", "m.thread_id = t.id")
    );
    let schema = mail_schema().scope(scope);
    let c = compile(&schema, "from:a invoice -label:x -spam").unwrap();
    let parent = format!(
        "EXISTS (SELECT 1 FROM messages m WHERE m.thread_id = t.id AND {FTS_IN} AND ({SENDER_LIKE}))"
    );
    let no_spam =
        format!("NOT EXISTS (SELECT 1 FROM messages m WHERE m.thread_id = t.id AND ({FTS_IN}))");
    let no_label =
        "NOT EXISTS (SELECT 1 FROM messages m WHERE m.thread_id = t.id AND (m.label = ?))";
    assert_eq!(
        c.where_sql,
        format!("{parent} AND {no_spam} AND {no_label}")
    );
    assert_eq!(
        c.params,
        vec![
            text("\"invoice\"*"),
            text("%a%"),
            text("\"spam\"*"),
            text("x")
        ],
        "positive terms co-occur in one child; each negated term is its own NOT EXISTS"
    );

    // A negated free-text term can also LIKE-match listed columns, escaped by default.
    let like = mail_schema().scope(
        ChildScope::new("messages m", "m.thread_id = t.id")
            .negated_text_like(&["m.subject", "m.snippet"]),
    );
    let c = compile(&like, "-100%").unwrap();
    assert!(c
        .where_sql
        .contains("m.subject LIKE ? ESCAPE '\\' OR m.snippet LIKE ? ESCAPE '\\'"));
    assert_eq!(
        c.params,
        vec![text("%100\\%%"), text("%100\\%%"), text("\"100%\"*")]
    );
    let unescaped = mail_schema().scope(
        ChildScope::new("messages m", "m.thread_id = t.id")
            .negated_text_like(&["m.subject"])
            .negated_text_unescaped(),
    );
    let c = compile(&unescaped, "-100%").unwrap();
    assert!(c.where_sql.contains("m.subject LIKE ?") && !c.where_sql.contains("ESCAPE"));
    assert_eq!(
        c.params[0],
        text("%100%%"),
        "the term keeps its LIKE metacharacters"
    );

    // An operator can be evaluated over another child table.
    let sourced = Schema::new()
        .fts("messages_fts", "m.rowid")
        .bind("label", Binding::Eq("l.name"))
        .bind("from", Binding::Like("m.sender"))
        .scope(
            ChildScope::new("messages m", "m.thread_id = t.id").op_source(
                "label",
                "thread_labels l",
                "l.thread_id = t.id",
            ),
        );
    let c = compile(&sourced, "label:work -label:old").unwrap();
    assert!(c.where_sql.contains(
        "EXISTS (SELECT 1 FROM thread_labels l WHERE l.thread_id = t.id AND (l.name = ?))"
    ));
    assert!(c.where_sql.contains(
        "NOT EXISTS (SELECT 1 FROM thread_labels l WHERE l.thread_id = t.id AND (l.name = ?))"
    ));
    assert!(
        !c.where_sql.contains("FROM messages m"),
        "label terms never touch the message source"
    );
    assert_eq!(c.params, vec![text("work"), text("old")]);
    assert_eq!(
        compile(&sourced, "label:a OR label:b").unwrap_err(),
        CompileError::OpSourceInOrGroup("label".to_owned())
    );
}

fn snippets() {
    assert_eq!((MATCH_START, MATCH_END), ('\u{1}', '\u{2}'));
    assert_eq!(
        fts_snippet_expr("messages_fts", 12),
        "snippet(messages_fts, -1, '\u{1}', '\u{2}', '...', 12)"
    );
    let span = |t: &str, matched| Span {
        text: t.to_owned(),
        matched,
    };
    assert_eq!(
        split_highlights("a \u{1}b\u{2} c"),
        vec![span("a ", false), span("b", true), span(" c", false)]
    );
    assert_eq!(split_highlights("plain"), vec![span("plain", false)]);
    assert_eq!(split_highlights("\u{1}all\u{2}"), vec![span("all", true)]);
    assert_eq!(split_highlights(""), vec![]);

    let terms = vec!["NEEDLE".to_owned()];
    assert_eq!(
        excerpt_around("  short text  ", &["short".to_owned()], 100),
        "short text"
    );
    let long = format!("{}needle{}", "x ".repeat(60), " y".repeat(60));
    let excerpt = excerpt_around(&long, &terms, 30);
    assert!(
        excerpt.starts_with("...") && excerpt.ends_with("..."),
        "{excerpt}"
    );
    assert!(excerpt.contains("needle"), "{excerpt}");
    assert!(excerpt.chars().count() <= 30 + 6, "{excerpt}");
    let head = format!("needle{}", " y".repeat(60));
    let excerpt = excerpt_around(&head, &terms, 30);
    assert!(
        excerpt.starts_with("needle") && excerpt.ends_with("..."),
        "{excerpt}"
    );

    assert!(contains_any_term("Hello World", &["WORLD".to_owned()]));
    assert!(!contains_any_term("Hello World", &["xyz".to_owned()]));
    assert!(
        !contains_any_term("Hello --", &["--".to_owned()]),
        "a term without letters or digits never matches"
    );
}

fn folding() {
    assert_eq!(fold("CAFÉ"), "café");
    assert_eq!(fold("Straße"), "strasse");
    assert_eq!(fold("STRASSE"), fold("straße"));
    assert_eq!(fold("ΣΑΣ"), fold("σας"), "final sigma folds to sigma");
    assert_eq!(fold("ﬃ"), "ffi");
    assert_eq!(fold("ſ"), "s");
    assert_eq!(fold("İ"), "i\u{307}");
    assert_ne!(fold("e\u{301}"), fold("é"), "no Unicode normalisation");
    assert_eq!(fold(""), "");
    assert_eq!(fold("東京"), "東京");
    assert_eq!(fold_with("ISPARTA", FoldOptions::turkic()), "ısparta");
    assert_eq!(fold_with("İstanbul", FoldOptions::turkic()), "istanbul");
    assert_eq!(fold_with("ISPARTA", FoldOptions::new()), "isparta");
    for text in ["CAFÉ", "Straße", "ΣΑΣ", "ﬃ", "İ", "東京", "ISPARTA"] {
        for options in [FoldOptions::new(), FoldOptions::turkic()] {
            let total: usize = text.chars().map(|c| folded_len(c, options)).sum();
            assert_eq!(
                total,
                fold_with(text, options).len(),
                "folded length of {text:?}"
            );
        }
    }
    assert_eq!(folded_len('ß', FoldOptions::new()), 2, "ß folds to ss");
    assert_eq!(FoldOptions::new(), FoldOptions::default());
    assert_ne!(FoldOptions::turkic(), FoldOptions::new());

    assert_eq!(FOLD_VERSION, 2);
    assert!(check_fold_version(2).is_ok());
    let stale = check_fold_version(1).unwrap_err();
    assert_eq!((stale.stored, stale.current), (1, 2));
    assert!(stale.to_string().contains("rebuild it"), "{stale}");
    let as_error: &dyn std::error::Error = &stale;
    assert!(!as_error.to_string().is_empty());
}

fn matcher_for(input: &str) -> TextMatcher {
    TextMatcher::new(&Query::parse(input, &syntax()).expect("fixture query parses"))
        .expect("matcher builds")
}

fn hit(input: &str, haystack: &str) -> bool {
    matcher_for(input).matches_with(&[haystack], Vec::new)
}

fn matching() {
    assert!(hit("Café", "CAFÉ"));
    assert!(hit("東京", "I went to 東京 last year"));
    assert!(hit("anscr", "Transcript"), "free text is a substring match");
    assert!(hit("strasse", "Hauptstraße 5"));
    assert!(hit("\"alpha beta\"", "x alpha beta y"));
    assert!(
        !hit("\"alpha beta\"", "alpha x beta"),
        "a phrase is contiguous"
    );
    assert!(
        hit("100%", "100% sure") && !hit("100%", "100 sure"),
        "% is an ordinary character"
    );
    assert!(
        hit("a_b", "the a_b file") && !hit("a_b", "the axb file"),
        "_ is an ordinary character"
    );
    assert!(hit("(alpha OR beta)", "only beta here") && !hit("(alpha OR beta)", "gamma"));
    assert!(!hit("alpha -gamma", "alpha gamma"));
    assert!(hit("alpha -gamma", "alpha delta"));

    let both = matcher_for("alpha beta");
    assert!(!both.matches_with(&["alpha only"], Vec::new));
    assert!(
        both.matches_with(&["alpha here"], || vec!["beta there".to_owned()]),
        "AND across haystacks"
    );
    let negated = matcher_for("alpha -gamma");
    assert!(
        !negated.matches_with(&["alpha"], || vec!["gamma".to_owned()]),
        "a negated term is checked in every haystack"
    );
    assert!(negated.matches_with(&["alpha"], || vec!["delta".to_owned()]));

    let called = Cell::new(false);
    let lazy = || {
        called.set(true);
        Vec::new()
    };
    assert!(matcher_for("alpha").matches_with(&["alpha"], lazy));
    assert!(
        !called.get(),
        "the lazy haystacks are not fetched when the eager ones decide"
    );
    let called = Cell::new(false);
    let lazy = || {
        called.set(true);
        vec!["alpha".to_owned()]
    };
    assert!(matcher_for("alpha").matches_with(&["nothing"], lazy));
    assert!(
        called.get(),
        "they are fetched when the eager ones cannot decide"
    );

    assert!(matcher_for("").is_empty());
    assert!(matcher_for("").matches_with(&["anything"], Vec::new));
    assert!(!matcher_for("alpha").is_empty());
    let malformed = TextMatcher::new(&Query::parse_lenient("(", &syntax())).unwrap();
    assert!(!malformed.is_empty());
    assert!(
        !malformed.matches_with(&["anything"], Vec::new),
        "a malformed query matches nothing"
    );
    // Operator terms are not free text.
    assert!(matcher_for("from:alice").is_empty());
    assert_eq!(
        MatchError::MixedOrGroup.to_string(),
        "an OR group mixes free text with operators or negation"
    );

    // Opt-in Turkic folding applies to needles and haystacks alike.
    let query = Query::parse("ISPARTA", &syntax()).unwrap();
    assert!(!TextMatcher::new(&query)
        .unwrap()
        .matches_with(&["ısparta"], Vec::new));
    assert!(TextMatcher::with_options(&query, FoldOptions::turkic())
        .unwrap()
        .matches_with(&["ısparta"], Vec::new));
}

fn matcher_snippets() {
    let m = matcher_for("needle");
    let span = |t: &str, matched| Span {
        text: t.to_owned(),
        matched,
    };
    let whole = m.snippet("a needle b", 100).expect("a hit");
    assert_eq!(
        whole.spans,
        vec![span("a ", false), span("needle", true), span(" b", false)]
    );
    assert!(!whole.cut_start && !whole.cut_end);

    let long = format!("{}Needle{}", "a ".repeat(40), " b".repeat(40));
    let cut = m.snippet(&long, 20).expect("a hit in a long text");
    assert!(cut.cut_start && cut.cut_end);
    let joined: String = cut.spans.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        joined.chars().count(),
        20,
        "the window is exactly max_chars"
    );
    assert!(
        long.contains(&joined),
        "the spans are a contiguous substring of the source"
    );
    assert!(
        cut.spans.iter().any(|s| s.matched && s.text == "Needle"),
        "the original casing is kept"
    );

    assert!(m.snippet("nothing here", 20).is_none());
    assert!(m.snippet("a needle b", 0).is_none());
    assert!(
        matcher_for("").snippet("a needle b", 20).is_none(),
        "no positive terms, no snippet"
    );
    assert!(
        matcher_for("-needle").snippet("a needle b", 20).is_none(),
        "negated terms are never highlighted"
    );
    let twice = matcher_for("ab cd").snippet("ab cd ab", 100).unwrap();
    assert_eq!(twice.spans.iter().filter(|s| s.matched).count(), 3);
}

fn paging() {
    let schema = mail_schema();
    let all = compile(&schema, "").unwrap();
    let keyset = Keyset::new().desc("received_at").desc("id");

    let first = all
        .page("SELECT id FROM messages", &keyset, None, 50)
        .unwrap();
    assert_eq!(
        first.sql,
        "SELECT id FROM messages WHERE (1=1) ORDER BY received_at DESC, id DESC LIMIT ?"
    );
    assert_eq!(
        first.params,
        vec![Param::Int(51)],
        "one extra row says whether another page exists"
    );

    let cursor = Cursor::new(vec![Param::Int(100), Param::Int(7)]);
    let next = all
        .page("SELECT id FROM messages", &keyset, Some(&cursor), 10)
        .unwrap();
    assert_eq!(
        next.sql,
        "SELECT id FROM messages WHERE (1=1) AND ((received_at < ?) OR (received_at = ? AND id < ?)) ORDER BY received_at DESC, id DESC LIMIT ?"
    );
    assert_eq!(
        next.params,
        vec![
            Param::Int(100),
            Param::Int(100),
            Param::Int(7),
            Param::Int(11)
        ]
    );

    let ascending = Keyset::new().asc("name").asc("id");
    let by_name = Cursor::new(vec![text("m"), Param::Int(3)]);
    let paged = all
        .page("SELECT id FROM t", &ascending, Some(&by_name), 5)
        .unwrap();
    assert!(
        paged
            .sql
            .contains("((name > ?) OR (name = ? AND id > ?)) ORDER BY name ASC, id ASC LIMIT ?"),
        "{}",
        paged.sql
    );
    assert_eq!(paged.params[0], text("m"));

    // The compiled predicate's own parameters come first.
    let filtered = compile(&schema, "label:work").unwrap();
    let paged = filtered
        .page("SELECT id FROM messages m", &keyset, Some(&cursor), 1)
        .unwrap();
    assert_eq!(
        paged.params,
        vec![
            text("work"),
            Param::Int(100),
            Param::Int(100),
            Param::Int(7),
            Param::Int(2)
        ]
    );

    // The range bound is redundant for output and adds a seekable bound on the first column.
    let bounded = Keyset::new().desc("received_at").desc("id").range_bound();
    let paged = all
        .page("SELECT id FROM t", &bounded, Some(&cursor), 10)
        .unwrap();
    assert!(
        paged
            .sql
            .contains("(1=1) AND (received_at <= ?) AND ((received_at < ?)"),
        "{}",
        paged.sql
    );
    assert_eq!(
        paged.params,
        vec![
            Param::Int(100),
            Param::Int(100),
            Param::Int(100),
            Param::Int(7),
            Param::Int(11)
        ],
        "the bound, then the cursor groups, then the limit"
    );
    assert_eq!(
        all.page("SELECT id FROM t", &bounded, None, 10)
            .unwrap()
            .sql,
        all.page("SELECT id FROM t", &keyset, None, 10).unwrap().sql,
        "with no cursor the bound adds nothing"
    );

    // A cursor-side expression replaces the plain placeholder for one column.
    let with_expr = Keyset::new().asc("received_at").asc("rowid").cursor_expr(
        1,
        "COALESCE((SELECT rowid FROM messages WHERE account_id = ? AND id = ?), -1)",
        vec![Param::Int(9)],
    );
    let by_id = Cursor::new(vec![Param::Int(5), text("m1")]);
    let paged = all
        .page("SELECT id FROM messages", &with_expr, Some(&by_id), 10)
        .unwrap();
    assert!(
        paged.sql.contains(
            "rowid > COALESCE((SELECT rowid FROM messages WHERE account_id = ? AND id = ?), -1)"
        ),
        "{}",
        paged.sql
    );
    assert_eq!(
        paged.params,
        vec![
            Param::Int(5),
            Param::Int(5),
            Param::Int(9),
            text("m1"),
            Param::Int(11)
        ]
    );

    // The clauses alone, for a caller-built statement.
    let clauses = all.page_clauses(&keyset, Some(&cursor)).unwrap();
    assert_eq!(clauses.order_by, "received_at DESC, id DESC");
    assert_eq!(
        clauses.params,
        vec![Param::Int(100), Param::Int(100), Param::Int(7)]
    );
    assert!(clauses.where_sql.starts_with("(1=1) AND ("));
    assert_eq!(all.page_clauses(&keyset, None).unwrap().where_sql, "(1=1)");

    // Refusals.
    assert_eq!(
        all.page("SELECT 1", &Keyset::new(), None, 1).unwrap_err(),
        CursorError::EmptyKeyset
    );
    let short = Cursor::new(vec![Param::Int(1)]);
    assert_eq!(
        all.page("SELECT 1", &keyset, Some(&short), 1).unwrap_err(),
        CursorError::Arity {
            expected: 2,
            got: 1
        }
    );
    let out_of_range = Keyset::new().asc("a").cursor_expr(5, "?", vec![]);
    assert_eq!(
        all.page("SELECT 1", &out_of_range, None, 1).unwrap_err(),
        CursorError::ColumnOutOfRange {
            column: 5,
            columns: 1
        }
    );
    let miscounted = Keyset::new().asc("a").cursor_expr(0, "? + ?", vec![]);
    assert_eq!(
        all.page("SELECT 1", &miscounted, None, 1).unwrap_err(),
        CursorError::ExprBindCount {
            column: 0,
            expected: 1,
            got: 2
        }
    );
    assert_eq!(CursorError::Malformed.to_string(), "malformed cursor");
    assert_eq!(
        CursorError::EmptyKeyset.to_string(),
        "keyset has no sort columns"
    );
    let as_error: &dyn std::error::Error = &CursorError::Malformed;
    assert!(!as_error.to_string().is_empty());
}

fn cursors() {
    let cursor = Cursor::new(vec![Param::Int(-5), text("héllo"), Param::Int(0)]);
    assert_eq!(cursor.values().len(), 3);
    let encoded = cursor.encode();
    assert_eq!(
        encoded, "k1i-5;t6:hélloi0;",
        "text is length-prefixed in bytes and needs no escaping"
    );
    assert_eq!(Cursor::decode(&encoded).unwrap(), cursor);
    assert_eq!(Cursor::decode("k1").unwrap().values(), []);
    let tricky = Cursor::new(vec![text("a;b:c\"d"), text("")]);
    assert_eq!(Cursor::decode(&tricky.encode()).unwrap(), tricky);
    let extreme = Cursor::new(vec![Param::Int(i64::MIN), Param::Int(i64::MAX)]);
    assert_eq!(Cursor::decode(&extreme.encode()).unwrap(), extreme);
    for bad in [
        "",
        "x1",
        "k2i1;",
        "k1i5",
        "k1i;",
        "k1i+5;",
        "k1ix;",
        "k1t3:ab",
        "k1tx:ab",
        "k1t:ab",
        "k1t1:é",
        "k1z",
        "k1t99999999999999999999:a",
    ] {
        assert_eq!(
            Cursor::decode(bad),
            Err(CursorError::Malformed),
            "must refuse {bad:?}"
        );
    }
}

fn key(x: &i64) -> Cursor {
    Cursor::new(vec![Param::Int(*x)])
}

fn source(data: &[i64], after: Option<&Cursor>, n: usize) -> Vec<i64> {
    let start = match after.map(Cursor::values) {
        None => 0,
        Some([Param::Int(v)]) => data.partition_point(|x| x <= v),
        Some(other) => panic!("unexpected cursor {other:?}"),
    };
    data[start..].iter().take(n).copied().collect()
}

fn scanning() {
    let data: Vec<i64> = (1..=10).collect();
    let largest_request = Cell::new(0);
    let fetch = |after: Option<&Cursor>, n: usize| -> Result<Vec<i64>, Infallible> {
        largest_request.set(largest_request.get().max(n));
        Ok(source(&data, after, n))
    };
    let even = |x: &i64| x % 2 == 0;

    let page = scan_page(None, 2, ScanBudget::new(100, 3), fetch, key, even).unwrap();
    assert_eq!(page.hits, [2, 4]);
    assert_eq!(page.scanned, 4);
    assert_eq!(
        page.next,
        Some(key(&4)),
        "resume after the last row examined"
    );
    assert!(
        largest_request.get() <= 3,
        "a fetch never asks for more than the chunk"
    );

    let page = scan_page(page.next, 2, ScanBudget::new(100, 3), fetch, key, even).unwrap();
    assert_eq!(page.hits, [6, 8]);
    let page = scan_page(page.next, 2, ScanBudget::new(100, 3), fetch, key, even).unwrap();
    assert_eq!(page.hits, [10]);
    assert_eq!(page.next, None, "a short source ends the scan");
    assert_eq!(page.scanned, 2);

    // The budget bounds the rows examined even when nothing matches.
    let page = scan_page(None, 5, ScanBudget::new(5, 2), fetch, key, |_| false).unwrap();
    assert!(page.hits.is_empty());
    assert_eq!(page.scanned, 5);
    assert_eq!(page.next, Some(key(&5)));
    // Zero budgets and limits are raised to one so every call makes progress.
    let page = scan_page(None, 0, ScanBudget::new(0, 0), fetch, key, |_| true).unwrap();
    assert_eq!((page.hits.len(), page.scanned), (1, 1));
    assert_eq!(ScanBudget::new(3, 2), ScanBudget::new(3, 2));

    // Draining page by page sees every hit once, in order.
    let mut all = Vec::new();
    let mut cursor = None;
    loop {
        let page = scan_page(cursor, 3, ScanBudget::new(4, 2), fetch, key, even).unwrap();
        all.extend(page.hits);
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(all, [2, 4, 6, 8, 10]);

    // A fetch error is returned as-is.
    let failing = |_: Option<&Cursor>, _: usize| -> Result<Vec<i64>, &'static str> { Err("boom") };
    assert_eq!(
        scan_page(None, 1, ScanBudget::new(10, 2), failing, key, even).unwrap_err(),
        "boom"
    );
}

fn reading_text_prefix() {
    let dir = std::env::temp_dir().join(format!("rk-search-consumer-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create consumer temp dir");
    let file = dir.join("body.txt");
    std::fs::write(&file, "ab\u{20ac}".as_bytes()).expect("write fixture");
    assert_eq!(read_text_prefix(&file, 100).unwrap(), "ab\u{20ac}");
    assert_eq!(read_text_prefix(&file, 5).unwrap(), "ab\u{20ac}");
    assert_eq!(
        read_text_prefix(&file, 4).unwrap(),
        "ab",
        "a cut inside a character drops the partial character"
    );
    assert_eq!(read_text_prefix(&file, 3).unwrap(), "ab");
    assert_eq!(read_text_prefix(&file, 2).unwrap(), "ab");
    assert_eq!(read_text_prefix(&file, 1).unwrap(), "a");
    assert_eq!(read_text_prefix(&file, 0).unwrap(), "");
    std::fs::write(&file, [b'a', 0xff, b'b']).expect("write invalid fixture");
    assert_eq!(
        read_text_prefix(&file, 10).unwrap(),
        "a\u{fffd}b",
        "invalid bytes become U+FFFD"
    );
    let missing = read_text_prefix(&dir.join("absent.txt"), 10).unwrap_err();
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
    std::fs::remove_dir_all(&dir).expect("remove consumer temp dir");
}

fn path(parts: &str) -> PathBuf {
    parts.split('/').collect()
}

fn entry(parts: &str, kind: Kind, size: Option<u64>) -> Entry {
    Entry {
        path: path(parts),
        kind,
        size,
    }
}

fn names(page: &Page) -> Vec<String> {
    page.items
        .iter()
        .map(|e| e.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

fn filename_index() {
    assert_eq!(
        (
            MAX_QUERY_LENGTH,
            MAX_EXTENSION_LENGTH,
            MAX_PAGE_LIMIT,
            MAX_PAGE_OFFSET
        ),
        (256, 64, 1_000, 1_000_000)
    );
    let mut index = FilenameIndex::new();
    assert!(index.is_empty());
    for e in [
        entry("docs", Kind::Dir, None),
        entry("docs/Reports", Kind::Dir, None),
        entry("docs/Reports/Q1-Report.PDF", Kind::File, Some(5_000)),
        entry("docs/Reports/q2-report.txt", Kind::File, Some(100)),
        entry("docs/notes.md", Kind::File, Some(5)),
        entry("docs/unsized.bin", Kind::File, None),
        entry("docs/Stra\u{df}e.txt", Kind::File, Some(1)),
        entry(".hidden-report", Kind::File, Some(50)),
    ] {
        index.insert(e);
    }
    assert_eq!(index.len(), 8);
    assert!(!index.is_empty());

    let page = index.search(&Request::new("REPORT", 10)).unwrap();
    assert_eq!(
        names(&page),
        [
            ".hidden-report",
            "Reports",
            "Q1-Report.PDF",
            "q2-report.txt"
        ]
    );
    assert_eq!(
        (page.total_matches, page.offset, page.limit, page.has_more),
        (4, 0, 10, false)
    );

    let by_kind = Request {
        query: "report".to_owned(),
        kind: Some(Kind::File),
        ..Request::default()
    };
    assert_eq!(
        names(&index.search(&by_kind).unwrap()),
        [".hidden-report", "Q1-Report.PDF", "q2-report.txt"]
    );
    for extension in ["pdf", ".PDF"] {
        let request = Request {
            extension: Some(extension.to_owned()),
            ..Request::default()
        };
        assert_eq!(names(&index.search(&request).unwrap()), ["Q1-Report.PDF"]);
    }
    let big = Request {
        min_size: Some(1_000),
        ..Request::default()
    };
    assert_eq!(names(&index.search(&big).unwrap()), ["Q1-Report.PDF"]);
    let small = Request {
        max_size: Some(100),
        ..Request::default()
    };
    assert_eq!(
        names(&index.search(&small).unwrap()),
        [
            ".hidden-report",
            "q2-report.txt",
            "Stra\u{df}e.txt",
            "notes.md"
        ],
        "path order, and an unknown size never satisfies a size predicate"
    );
    let band = Request {
        min_size: Some(5),
        max_size: Some(100),
        ..Request::default()
    };
    assert_eq!(index.search(&band).unwrap().total_matches, 3);
    assert_eq!(
        names(&index.search(&Request::new("STRASSE", 5)).unwrap()),
        ["Stra\u{df}e.txt"],
        "names are case-folded"
    );
    assert_eq!(
        index
            .search(&Request::new("no-such-name", 5))
            .unwrap()
            .items,
        vec![]
    );

    // Pagination.
    let everything = index.search(&Request::new("", 100)).unwrap();
    assert_eq!(
        everything.total_matches, 8,
        "an empty query matches every name, hidden ones included"
    );
    let middle = index
        .search(&Request {
            offset: 1,
            limit: 2,
            ..Request::default()
        })
        .unwrap();
    assert_eq!(middle.items, everything.items[1..3].to_vec());
    assert!(middle.has_more);
    assert_eq!(middle.total_matches, 8);
    let last = index
        .search(&Request {
            offset: 6,
            limit: 5,
            ..Request::default()
        })
        .unwrap();
    assert_eq!(last.items.len(), 2);
    assert!(!last.has_more);
    let beyond = index
        .search(&Request {
            offset: 50,
            limit: 5,
            ..Request::default()
        })
        .unwrap();
    assert!(beyond.items.is_empty() && !beyond.has_more);

    // Validation.
    assert_eq!(
        index.search(&Request::new("a", 0)).unwrap_err(),
        FilenameError::InvalidLimit(0)
    );
    assert_eq!(
        index
            .search(&Request::new("a", MAX_PAGE_LIMIT + 1))
            .unwrap_err(),
        FilenameError::InvalidLimit(MAX_PAGE_LIMIT + 1)
    );
    assert_eq!(
        FilenameError::InvalidLimit(0).to_string(),
        "limit 0 must be 1..=1000"
    );
    assert_eq!(
        index
            .search(&Request {
                offset: MAX_PAGE_OFFSET + 1,
                ..Request::default()
            })
            .unwrap_err(),
        FilenameError::InvalidOffset(MAX_PAGE_OFFSET + 1)
    );
    assert!(matches!(
        index.search(&Request::new("a\0b", 5)),
        Err(FilenameError::InvalidQuery(_))
    ));
    assert!(matches!(
        index.search(&Request::new("x".repeat(MAX_QUERY_LENGTH + 1), 5)),
        Err(FilenameError::InvalidQuery(_))
    ));
    assert!(index
        .search(&Request::new("x".repeat(MAX_QUERY_LENGTH), 5))
        .is_ok());
    for bad in ["", ".", "a/b", "a\\b", "a\0"] {
        let request = Request {
            extension: Some(bad.to_owned()),
            ..Request::default()
        };
        assert!(
            matches!(
                index.search(&request),
                Err(FilenameError::InvalidExtension(_))
            ),
            "must refuse extension {bad:?}"
        );
    }
    let inverted = Request {
        min_size: Some(10),
        max_size: Some(1),
        ..Request::default()
    };
    assert_eq!(
        index.search(&inverted).unwrap_err(),
        FilenameError::InvalidSizeRange
    );
    assert_eq!(
        FilenameError::InvalidSizeRange.to_string(),
        "min_size exceeds max_size"
    );

    // De-duplication by path, then removing a whole tree.
    let mut again = FilenameIndex::default();
    again.insert(entry("a/b.txt", Kind::File, Some(1)));
    again.insert(entry("a/b.txt", Kind::File, Some(1)));
    assert_eq!(again.len(), 2);
    assert_eq!(
        again.search(&Request::new("b", 5)).unwrap().total_matches,
        1
    );
    assert_eq!(again.len(), 1, "a search drops duplicate paths");

    assert_eq!(index.remove_tree(&path("docs")), 7);
    assert_eq!(index.len(), 1);
    assert_eq!(
        names(&index.search(&Request::new("", 5)).unwrap()),
        [".hidden-report"]
    );
    assert_eq!(index.remove_tree(&path("docs")), 0);
}

// ---- the "sqlite" feature: Param binds as rusqlite::ToSql and compiled SQL runs on a real SQLite ----

const MAR1: i64 = 1_709_251_200_000; // 2024-03-01
const APR1: i64 = 1_711_929_600_000; // 2024-04-01

struct Message {
    id: &'static str,
    thread: i64,
    sender: &'static str,
    subject: &'static str,
    body: &'static str,
    label: &'static str,
    size: i64,
    received_at: i64,
    unread: i64,
    starred: i64,
    parts: i64,
    msgid: &'static str,
    rcpt: &'static str,
    cc: Option<&'static str>,
}

fn messages() -> Vec<Message> {
    vec![
        Message {
            id: "m1",
            thread: 1,
            sender: "alice@acme.com",
            subject: "March invoice",
            body: "Please find the invoice for March attached. Payment due in 30 days.",
            label: "INBOX",
            size: 120_000,
            received_at: MAR1,
            unread: 1,
            starred: 0,
            parts: 2,
            msgid: "id1@acme",
            rcpt: "bob@example.org",
            cc: Some("ann@acme.com"),
        },
        Message {
            id: "m2",
            thread: 1,
            sender: "bob@example.org",
            subject: "Lunch on Friday",
            body: "Want to grab lunch on Friday? The new ramen place is open.",
            label: "INBOX",
            size: 2_000,
            received_at: MAR1 + DAY,
            unread: 0,
            starred: 1,
            parts: 1,
            msgid: "id2@example",
            rcpt: "alice@acme.com",
            cc: None,
        },
        Message {
            id: "m3",
            thread: 2,
            sender: "alice@acme.com",
            subject: "Quarterly report",
            body: "The quarterly report is ready. Revenue grew twelve percent versus last quarter.",
            label: "ARCHIVE",
            size: 5_500_000,
            received_at: APR1,
            unread: 0,
            starred: 0,
            parts: 3,
            msgid: "id3@acme",
            rcpt: "carol@example.org",
            cc: Some("bob@example.org"),
        },
        Message {
            id: "m4",
            thread: 3,
            sender: "carol@example.org",
            subject: "Receipt for lunch",
            body: "Your receipt for lunch is attached. Thank you for dining with us.",
            label: "INBOX",
            size: 9_000,
            received_at: APR1 + DAY,
            unread: 1,
            starred: 0,
            parts: 1,
            msgid: "id4@example",
            rcpt: "bob@example.org",
            cc: None,
        },
        Message {
            id: "m5",
            thread: 3,
            sender: "50%_off@promo.test",
            subject: "Sale",
            body: "A literal percent and underscore in the sender for escaping.",
            label: "SPAM",
            size: 800,
            received_at: APR1,
            unread: 1,
            starred: 1,
            parts: 1,
            msgid: "id5@promo",
            rcpt: "alice@acme.com",
            cc: Some("dave@acme.com"),
        },
    ]
}

fn mail_db() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory database");
    conn.execute_batch(
        "CREATE TABLE threads(id INTEGER PRIMARY KEY);
         CREATE TABLE messages(
             id TEXT PRIMARY KEY, thread_id INTEGER, sender TEXT, subject TEXT, body TEXT, label TEXT,
             size INTEGER, received_at INTEGER, unread INTEGER, starred INTEGER, parts INTEGER,
             msgid TEXT, rcpt TEXT, cc TEXT);
         CREATE VIRTUAL TABLE messages_fts USING fts5(subject, body, content='messages', content_rowid='rowid');",
    )
    .expect("create schema");
    for m in messages() {
        conn.execute(
            "INSERT INTO messages(id, thread_id, sender, subject, body, label, size, received_at, unread, starred, parts, msgid, rcpt, cc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                m.id,
                m.thread,
                m.sender,
                m.subject,
                m.body,
                m.label,
                m.size,
                m.received_at,
                m.unread,
                m.starred,
                m.parts,
                m.msgid,
                m.rcpt,
                m.cc
            ],
        )
        .expect("insert message");
    }
    conn.execute_batch(
        "INSERT INTO threads(id) VALUES (1), (2), (3);
         INSERT INTO messages_fts(rowid, subject, body) SELECT rowid, subject, body FROM messages;",
    )
    .expect("index messages");
    conn
}

/// Compile `input` under `schema` at the fixed clock, bind it with the crate's own ToSql, run it.
fn ids_for(
    conn: &Connection,
    schema: &Schema,
    select: &str,
    order: &str,
    input: &str,
) -> Vec<String> {
    let compiled = schema
        .compile_at(
            &schema.parse(input).expect("fixture query parses"),
            APR1 + 10 * DAY,
        )
        .unwrap_or_else(|e| panic!("compile {input:?}: {e}"));
    let sql = format!("{} {order}", compiled.select(select));
    let mut statement = conn
        .prepare(&sql)
        .unwrap_or_else(|e| panic!("prepare {sql}: {e}"));
    let rows = statement
        .query_map(params_from_iter(compiled.params.iter()), |row| {
            row.get::<_, String>(0)
        })
        .unwrap_or_else(|e| panic!("run {input:?}: {e}"));
    rows.map(|row| row.unwrap()).collect()
}

fn sqlite_execution() {
    // Param is a ToSql: Text and Int bind as their SQLite types.
    let conn = Connection::open_in_memory().unwrap();
    let (kind, value): (String, i64) = conn
        .query_row(
            "SELECT typeof(?1), ?2",
            rusqlite::params![&text("x"), &Param::Int(7)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((kind.as_str(), value), ("text", 7));
    let kind: String = conn
        .query_row("SELECT typeof(?1)", [&Param::Int(7)], |row| row.get(0))
        .unwrap();
    assert_eq!(kind, "integer");

    let conn = mail_db();
    let schema = mail_schema();
    let ids = |input: &str| {
        ids_for(
            &conn,
            &schema,
            "SELECT m.id FROM messages m",
            "ORDER BY m.id",
            input,
        )
    };

    // Free text through FTS5, with prefix matching, a phrase and negation.
    assert_eq!(ids(""), ["m1", "m2", "m3", "m4", "m5"]);
    assert_eq!(ids("invoice"), ["m1"]);
    assert_eq!(ids("inv"), ["m1"], "a trailing prefix wildcard");
    assert_eq!(ids("lunch"), ["m2", "m4"]);
    assert_eq!(ids("\"quarterly report\""), ["m3"]);
    assert_eq!(ids("lunch -receipt"), ["m2"]);
    assert_eq!(ids("-lunch"), ["m1", "m3", "m5"]);
    assert_eq!(ids("(invoice OR ramen)"), ["m1", "m2"]);
    assert_eq!(ids("nosuchword"), Vec::<String>::new());

    // LIKE operators escape wildcards, so `_` and `%` are ordinary characters.
    assert_eq!(ids("from:alice"), ["m1", "m3"]);
    assert_eq!(ids("from:50%_off"), ["m5"]);
    assert_eq!(
        ids("from:5_%"),
        Vec::<String>::new(),
        "an unescaped wildcard would have matched m5"
    );
    assert_eq!(
        ids("-from:alice"),
        ["m2", "m4", "m5"],
        "negation is NULL-safe"
    );

    // Equality, comparisons for every value kind, flags and ids.
    assert_eq!(ids("label:INBOX"), ["m1", "m2", "m4"]);
    assert_eq!(ids("-label:INBOX"), ["m3", "m5"]);
    assert_eq!(ids("size:5k"), ["m1", "m3", "m4"]);
    assert_eq!(ids("after:2024-04-01"), ["m3", "m4", "m5"]);
    assert_eq!(
        ids("newer:30d"),
        ["m3", "m4", "m5"],
        "relative to the explicit clock"
    );
    assert_eq!(ids("count:1"), ["m2", "m4", "m5"]);
    assert_eq!(ids("is:unread"), ["m1", "m4", "m5"]);
    assert_eq!(ids("-is:unread"), ["m2", "m3"]);
    assert_eq!(ids("is:starred"), ["m2", "m5"]);
    assert_eq!(ids("id:<id3@acme>"), ["m3"]);

    // Shape-dependent and multi-bind operators.
    assert_eq!(ids("to:@acme.com"), ["m2", "m5"]);
    assert_eq!(ids("to:Bob@Example.org"), ["m1", "m4"]);
    assert_eq!(ids("to:carol"), ["m3"]);
    assert_eq!(ids("cc:ann"), ["m1"]);
    assert_eq!(
        ids("cc:example.org"),
        ["m3"],
        "the suffix half of the multi-bind"
    );

    // Combinations: AND across terms, OR groups over operators, a mixed group with free text.
    assert_eq!(ids("invoice from:alice label:INBOX"), ["m1"]);
    assert_eq!(ids("from:alice OR from:bob"), ["m1", "m2", "m3"]);
    assert_eq!(ids("from:carol OR ramen"), ["m2", "m4"]);
    assert_eq!(ids("lunch size:5k"), ["m4"]);
    assert_eq!(
        schema
            .compile_at(&Query::parse_lenient("(", &schema.syntax()), NOW)
            .map(|c| ids_for_compiled(&conn, &c))
            .unwrap(),
        Vec::<String>::new(),
        "a malformed lenient query restricts to zero rows"
    );

    // A parent/child scope runs: positive terms co-occur in one child, each negation is its own.
    let threads = mail_schema().scope(ChildScope::new("messages m", "m.thread_id = t.id"));
    let thread_ids = |input: &str| {
        ids_for(
            &conn,
            &threads,
            "SELECT CAST(t.id AS TEXT) FROM threads t",
            "ORDER BY t.id",
            input,
        )
    };
    assert_eq!(thread_ids("from:alice invoice"), ["1"]);
    assert_eq!(
        thread_ids("from:bob invoice"),
        Vec::<String>::new(),
        "no single message has both"
    );
    assert_eq!(thread_ids("lunch"), ["1", "3"]);
    assert_eq!(thread_ids("-lunch"), ["2"]);
    assert_eq!(thread_ids("is:starred -label:SPAM"), ["1"]);

    // FTS5 snippets: the expression runs and the highlight markers split into spans.
    let snippet = format!(
        "SELECT {} FROM messages_fts WHERE messages_fts MATCH ?1",
        fts_snippet_expr("messages_fts", 12)
    );
    let mut matched = Vec::new();
    let mut statement = conn.prepare(&snippet).unwrap();
    let rows = statement
        .query_map(["\"invoice\"*"], |row| row.get::<_, String>(0))
        .unwrap();
    for row in rows {
        for span in split_highlights(&row.unwrap()) {
            if span.matched {
                matched.push(span.text.to_lowercase());
            }
        }
    }
    assert!(
        !matched.is_empty() && matched.iter().all(|m| m == "invoice"),
        "{matched:?}"
    );

    // Keyset paging walks every row once, in order, across cursor text round trips.
    let walk =
        |schema: &Schema, input: &str, keyset: &Keyset, page_size: usize| -> Vec<Vec<String>> {
            let compiled = schema
                .compile_at(&schema.parse(input).unwrap(), APR1 + 10 * DAY)
                .unwrap();
            let mut pages = Vec::new();
            let mut cursor: Option<Cursor> = None;
            loop {
                let page = compiled
                    .page(
                        "SELECT m.id, m.received_at FROM messages m",
                        keyset,
                        cursor.as_ref(),
                        page_size,
                    )
                    .unwrap();
                let mut statement = conn
                    .prepare(&page.sql)
                    .unwrap_or_else(|e| panic!("prepare {}: {e}", page.sql));
                let rows: Vec<(String, i64)> = statement
                    .query_map(params_from_iter(page.params.iter()), |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                    .unwrap()
                    .map(|row| row.unwrap())
                    .collect();
                let more = rows.len() > page_size;
                let kept = &rows[..rows.len().min(page_size)];
                pages.push(kept.iter().map(|(id, _)| id.clone()).collect());
                match (more, kept.last()) {
                    (true, Some((id, received_at))) => {
                        let next = Cursor::new(vec![Param::Int(*received_at), text(id)]);
                        cursor = Some(Cursor::decode(&next.encode()).unwrap());
                    }
                    _ => return pages,
                }
            }
        };
    let newest_first = Keyset::new().desc("received_at").desc("id");
    assert_eq!(
        walk(&schema, "", &newest_first, 2),
        [vec!["m4", "m5"], vec!["m3", "m2"], vec!["m1"]]
    );
    assert_eq!(
        walk(&schema, "", &newest_first.clone().range_bound(), 2),
        [vec!["m4", "m5"], vec!["m3", "m2"], vec!["m1"]],
        "the redundant range bound changes no output"
    );
    assert_eq!(
        walk(&schema, "label:INBOX", &newest_first, 2),
        [vec!["m4", "m2"], vec!["m1"]]
    );
    assert_eq!(
        walk(&schema, "", &Keyset::new().asc("received_at").asc("id"), 4),
        [vec!["m1", "m2", "m3", "m5"], vec!["m4"]]
    );
    assert_eq!(
        walk(&schema, "nosuchword", &newest_first, 2),
        [Vec::<String>::new()]
    );
}

fn ids_for_compiled(conn: &Connection, compiled: &rightkit_search::Compiled) -> Vec<String> {
    let sql = format!(
        "{} ORDER BY m.id",
        compiled.select("SELECT m.id FROM messages m")
    );
    let mut statement = conn.prepare(&sql).unwrap();
    let rows = statement
        .query_map(params_from_iter(compiled.params.iter()), |row| {
            row.get::<_, String>(0)
        })
        .unwrap();
    rows.map(|row| row.unwrap()).collect()
}

/// The crate root re-exports items that live in public modules. Naming the modules here makes a module
/// that stops being public, or an item that moves, fail to compile.
fn module_paths() {
    let _: Option<rightkit_search::fold::FoldOptions> = None::<FoldOptions>;
    let _: Option<rightkit_search::fold::RebuildNeeded> = None::<rightkit_search::RebuildNeeded>;
    assert_eq!(rightkit_search::fold::FOLD_VERSION, FOLD_VERSION);
    assert_eq!(rightkit_search::fold::fold("ABC"), fold("ABC"));
    assert_eq!(
        rightkit_search::fold::fold_with("ABC", FoldOptions::new()),
        fold("ABC")
    );
    assert!(rightkit_search::fold::check_fold_version(FOLD_VERSION).is_ok());
    let _: Option<rightkit_search::matcher::TextMatcher> = None::<TextMatcher>;
    let _: Option<rightkit_search::matcher::MatchError> = None::<MatchError>;
    let _: Option<rightkit_search::matcher::Snippet> = None::<rightkit_search::Snippet>;
    let _: Option<rightkit_search::sql::Schema> = None::<Schema>;
    let _: Option<rightkit_search::sql::Binding> = None::<Binding>;
    let _: Option<rightkit_search::sql::ChildScope> = None::<ChildScope>;
    let _: Option<rightkit_search::sql::CompileError> = None::<CompileError>;
    let _: Option<rightkit_search::sql::Compiled> = None::<rightkit_search::Compiled>;
    let _: Option<rightkit_search::sql::OpSource> = None::<rightkit_search::OpSource>;
    let _: Option<rightkit_search::sql::Param> = None::<Param>;
    let _: Option<rightkit_search::sql::ShapeSql> = None::<ShapeSql>;
    let _: Option<rightkit_search::sql::ValueKind> = None::<ValueKind>;
    let _: Option<rightkit_search::terms::Query> = None::<Query>;
    let _: Option<rightkit_search::terms::OpTerm> = None::<OpTerm>;
    let _: Option<rightkit_search::page::PagedSql> = None::<rightkit_search::PagedSql>;
    let _: Option<rightkit_search::page::PageClauses> = None::<rightkit_search::PageClauses>;
    let _: Option<rightkit_search::scan::ScanPage<u8>> = None::<rightkit_search::ScanPage<u8>>;
    let _: Option<rightkit_search::ast::Syntax> = None::<rightkit_search::Syntax>;
    let _: Option<rightkit_search::ast::ParseError> = None::<rightkit_search::ParseError>;
}

fn main() {
    module_paths();
    lexer();
    parser();
    queries();
    values();
    compiler();
    child_scopes();
    snippets();
    folding();
    matching();
    matcher_snippets();
    paging();
    cursors();
    scanning();
    reading_text_prefix();
    filename_index();
    sqlite_execution();
    println!("OK rightkit-search (registry consumer)");
}
