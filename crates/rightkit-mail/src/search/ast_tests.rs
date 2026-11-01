//! Parser tests against `rightkit_search::ast`, using MailRight's operator
//! vocabulary from `query::syntax()`.

use rightkit_search::ast::{parse, Leaf, Node, ParseError};

fn leaf(op: Option<&str>, value: &str, negated: bool, is_phrase: bool) -> Leaf {
    Leaf {
        op: op.map(String::from),
        value: value.into(),
        negated,
        is_phrase,
    }
}

#[test]
fn parses_bare_words_as_anded_leaves() {
    let nodes = parse("invoice march", &super::query::syntax()).unwrap();
    assert_eq!(
        nodes,
        vec![
            Node::Leaf(leaf(None, "invoice", false, false)),
            Node::Leaf(leaf(None, "march", false, false)),
        ]
    );
}

#[test]
fn parses_quoted_phrase_as_phrase_leaf() {
    let nodes = parse("\"invoice march\"", &super::query::syntax()).unwrap();
    assert_eq!(
        nodes,
        vec![Node::Leaf(leaf(None, "invoice march", false, true))]
    );
}

#[test]
fn parses_operator_prefixed_phrase() {
    let nodes = parse("subject:\"foo bar\"", &super::query::syntax()).unwrap();
    assert_eq!(
        nodes,
        vec![Node::Leaf(leaf(Some("subject"), "foo bar", false, true))]
    );
}

#[test]
fn parses_negated_operator() {
    let nodes = parse("-from:alice", &super::query::syntax()).unwrap();
    assert_eq!(
        nodes,
        vec![Node::Leaf(leaf(Some("from"), "alice", true, false))]
    );
}

#[test]
fn parses_negated_bare_word() {
    let nodes = parse("-invoice", &super::query::syntax()).unwrap();
    assert_eq!(nodes, vec![Node::Leaf(leaf(None, "invoice", true, false))]);
}

#[test]
fn parses_negated_quoted_phrase() {
    let nodes = parse("-\"foo bar\"", &super::query::syntax()).unwrap();
    assert_eq!(nodes, vec![Node::Leaf(leaf(None, "foo bar", true, true))]);
    match &nodes[0] {
        Node::Leaf(l) => assert!(l.negated, "-\"foo bar\" must be negated"),
        other => panic!("expected a leaf, got {other:?}"),
    }
}

#[test]
fn parses_or_chain() {
    let nodes = parse("invoice OR receipt", &super::query::syntax()).unwrap();
    assert_eq!(
        nodes,
        vec![Node::Or(vec![
            leaf(None, "invoice", false, false),
            leaf(None, "receipt", false, false),
        ])]
    );
}

#[test]
fn parses_brace_or_group() {
    let nodes = parse("{ march lunch }", &super::query::syntax()).unwrap();
    assert_eq!(
        nodes,
        vec![Node::Or(vec![
            leaf(None, "march", false, false),
            leaf(None, "lunch", false, false),
        ])]
    );
}

#[test]
fn parenthesized_group_splices_into_parent_and_list() {
    let nodes = parse("(from:alice subject:invoice)", &super::query::syntax()).unwrap();
    assert_eq!(
        nodes,
        vec![
            Node::Leaf(leaf(Some("from"), "alice", false, false)),
            Node::Leaf(leaf(Some("subject"), "invoice", false, false)),
        ]
    );
}

#[test]
fn parenthesized_or_group_preserved_as_or_node() {
    let nodes = parse("(invoice OR receipt) from:alice", &super::query::syntax()).unwrap();
    assert_eq!(
        nodes,
        vec![
            Node::Or(vec![
                leaf(None, "invoice", false, false),
                leaf(None, "receipt", false, false),
            ]),
            Node::Leaf(leaf(Some("from"), "alice", false, false)),
        ]
    );
}

#[test]
fn nested_parens_supported() {
    let nodes = parse(
        "(from:alice (subject:invoice OR subject:receipt))",
        &super::query::syntax(),
    )
    .unwrap();
    assert_eq!(
        nodes,
        vec![
            Node::Leaf(leaf(Some("from"), "alice", false, false)),
            Node::Or(vec![
                leaf(Some("subject"), "invoice", false, false),
                leaf(Some("subject"), "receipt", false, false),
            ]),
        ]
    );
}

#[test]
fn unknown_operator_is_a_typed_error_not_silent_text() {
    let err = parse("bogus:value", &super::query::syntax()).unwrap_err();
    assert_eq!(
        err,
        ParseError::UnsupportedOperator {
            operator: "bogus".into(),
            raw: "bogus:value".into(),
        }
    );
}

#[test]
fn unknown_operator_inside_quoted_phrase_prefix_is_a_typed_error() {
    let err = parse("bogus:\"value here\"", &super::query::syntax()).unwrap_err();
    assert!(matches!(err, ParseError::UnsupportedOperator { operator, .. } if operator == "bogus"));
}

#[test]
fn known_new_operators_parse_without_error() {
    for q in [
        "size:5m",
        "larger:1k",
        "smaller:10m",
        "rfc822msgid:<abc@x.com>",
    ] {
        assert!(
            parse(q, &super::query::syntax()).is_ok(),
            "expected {q:?} to parse"
        );
    }
}

#[test]
fn unbalanced_paren_is_typed_error() {
    assert!(matches!(
        parse("(from:alice", &super::query::syntax()),
        Err(ParseError::UnbalancedGroup { .. })
    ));
}

#[test]
fn unbalanced_brace_is_typed_error() {
    assert!(matches!(
        parse("{ march", &super::query::syntax()),
        Err(ParseError::UnbalancedGroup { .. })
    ));
}

#[test]
fn stray_closing_paren_is_typed_error() {
    assert!(matches!(
        parse("from:alice)", &super::query::syntax()),
        Err(ParseError::UnbalancedGroup { .. })
    ));
}

#[test]
fn empty_group_is_typed_error() {
    assert!(matches!(
        parse("()", &super::query::syntax()),
        Err(ParseError::EmptyGroup)
    ));
    assert!(matches!(
        parse("{}", &super::query::syntax()),
        Err(ParseError::EmptyGroup)
    ));
}

#[test]
fn url_shaped_token_is_not_flagged_as_unknown_operator() {
    // "http" isn't a known operator, but "http://x.com" must not be
    // misclassified as an unsupported-operator attempt — plain text.
    // (':' is not in the leaf-cleaning strip set, only '"'/'*'/'('/')'.)
    let nodes = parse("http://x.com", &super::query::syntax()).unwrap();
    assert_eq!(
        nodes,
        vec![Node::Leaf(leaf(None, "http://x.com", false, false))]
    );
}

#[test]
fn stray_colon_in_free_text_is_not_flagged() {
    // "time:" has an empty rest after the colon, so it is plain text, and
    // "5pm" is a separate word token. Both survive as leaves.
    let nodes = parse("time: 5pm", &super::query::syntax()).unwrap();
    assert_eq!(nodes.len(), 2);
}

#[test]
fn mixed_and_or_and_negation() {
    let nodes = parse(
        "project (urgent OR blocked) -is:read",
        &super::query::syntax(),
    )
    .unwrap();
    assert_eq!(
        nodes,
        vec![
            Node::Leaf(leaf(None, "project", false, false)),
            Node::Or(vec![
                leaf(None, "urgent", false, false),
                leaf(None, "blocked", false, false),
            ]),
            Node::Leaf(leaf(Some("is"), "read", true, false)),
        ]
    );
}
