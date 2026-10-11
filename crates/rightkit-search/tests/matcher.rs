//! Lane A: free-text matching (`TextMatcher`), folding, literal queries and snippets.

use rightkit_search::{fold, MatchError, Query, Syntax, TextMatcher};
use std::cell::Cell;

fn syntax() -> Syntax {
    Syntax::new(["from", "label"])
}

fn parse(input: &str) -> Query {
    Query::parse(input, &syntax()).expect("query parses")
}

fn matcher(input: &str) -> TextMatcher {
    TextMatcher::new(&parse(input)).expect("matcher builds")
}

/// True when `input` matches one haystack, with no lazy haystacks.
fn hit(input: &str, text: &str) -> bool {
    matcher(input).matches_with(&[text], Vec::new)
}

#[test]
fn folds_unicode_case() {
    assert!(hit("Café", "CAFÉ"));
    assert!(hit("東京", "I went to 東京 last year"));
    assert!(hit("ÉCOLE", "une école"));
    assert_eq!(fold::fold("ÉCOLE"), "école");
}

#[test]
fn mid_word_fragment_matches() {
    assert!(hit("anscr", "Transcript"));
}

#[test]
fn terms_and_across_haystacks() {
    let m = matcher("alpha beta");
    assert!(!m.matches_with(&["alpha only"], Vec::new));
    assert!(m.matches_with(&["alpha here"], || vec!["beta there".to_string()]));
    assert!(m.matches_with(&["beta"], || vec!["alpha".to_string()]));
}

#[test]
fn phrase_is_contiguous() {
    assert!(hit("\"alpha beta\"", "x alpha beta y"));
    assert!(!hit("\"alpha beta\"", "alpha x beta"));
    assert!(!hit("\"alpha beta\"", "beta alpha"));
}

#[test]
fn negation_checks_every_haystack() {
    let m = matcher("alpha -gamma");
    // Negated term in the eager haystack: decided false.
    assert!(!m.matches_with(&["alpha gamma"], Vec::new));
    // Negated term only in a lazy haystack: must be seen.
    assert!(!m.matches_with(&["alpha"], || vec!["gamma".to_string()]));
    // Absent everywhere: match.
    assert!(m.matches_with(&["alpha"], || vec!["delta".to_string()]));
}

#[test]
fn percent_and_underscore_are_literal() {
    assert!(hit("100%", "100% sure"));
    assert!(!hit("100%", "100 sure"));
    assert!(hit("a_b", "the a_b file"));
    assert!(!hit("a_b", "the axb file"));
}

#[test]
fn lazy_not_called_when_eager_decides() {
    let called = Cell::new(false);
    let lazy = || {
        called.set(true);
        Vec::new()
    };

    // Positive term found eagerly, nothing negated: decided true.
    assert!(matcher("alpha").matches_with(&["alpha"], lazy));
    assert!(!called.get());

    // Negated term found eagerly: decided false.
    called.set(false);
    assert!(!matcher("alpha -gamma").matches_with(&["alpha gamma"], lazy));
    assert!(!called.get());

    // Positive term absent eagerly: lazy must be consulted.
    called.set(false);
    assert!(matcher("alpha").matches_with(&["beta"], || {
        called.set(true);
        vec!["alpha".to_string()]
    }));
    assert!(called.get());
}

#[test]
fn mixed_or_group_is_typed_error() {
    let err = TextMatcher::new(&parse("alpha OR from:bob")).unwrap_err();
    assert_eq!(err, MatchError::MixedOrGroup);
    assert!(!err.to_string().is_empty());

    // Free-text OR groups are fine.
    assert!(hit("(alpha OR beta)", "only beta here"));
    assert!(!hit("(alpha OR beta)", "gamma"));

    // Operator-only OR groups carry no free text, so the matcher is empty.
    assert!(TextMatcher::new(&parse("from:bob OR label:x"))
        .unwrap()
        .is_empty());
}

#[test]
fn literal_ignores_operators_and_quotes() {
    let q = Query::literal("  from:alice \"x y\" -b  ");
    assert_eq!(q.phrases, vec!["from:alice \"x y\" -b".to_string()]);
    assert!(q.ops.is_empty());
    assert!(q.text.is_empty());
    assert!(q.negated_text.is_empty());
    assert!(!q.is_empty());

    let m = TextMatcher::new(&q).unwrap();
    assert!(m.matches_with(&["see from:alice \"x y\" -b here"], Vec::new));
    assert!(!m.matches_with(&["alice"], Vec::new));

    assert!(Query::literal("   ").is_empty());
}

#[test]
fn filters_only_keeps_operators_and_drops_text() {
    let q = parse("from:alice hello \"big day\" -gone (label:x OR label:y)");
    let f = q.filters_only();
    assert_eq!(f.ops, q.ops);
    assert_eq!(f.op_or_groups, q.op_or_groups);
    assert!(f.text.is_empty());
    assert!(f.phrases.is_empty());
    assert!(f.negated_text.is_empty());
    assert!(f.or_groups.is_empty());
}

#[test]
fn snippet_spans_reassemble_to_a_substring_and_matched_spans_fold_to_a_term() {
    // "İ" folds to two characters, so offsets before the hit must still line up.
    let text =
        "İçinde bir sürü yazı var, sonra İstanbul geçiyor ve daha da uzun bir cümle devam eder";
    let needle = "İSTANBUL";
    let snippet = matcher(needle)
        .snippet(text, 24)
        .expect("text contains the needle");

    let reassembled: String = snippet.spans.iter().map(|s| s.text.as_str()).collect();
    assert!(
        text.contains(&reassembled),
        "spans must be a substring: {reassembled:?}"
    );
    assert!(reassembled.chars().count() <= 24);

    let matched: Vec<&str> = snippet
        .spans
        .iter()
        .filter(|s| s.matched)
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(matched.len(), 1, "one hit in the window: {matched:?}");
    assert_eq!(fold::fold(matched[0]), fold::fold(needle));

    assert!(snippet.cut_start);
    assert!(snippet.cut_end);
}

#[test]
fn snippet_absent_when_nothing_positive_occurs() {
    assert!(matcher("zzz").snippet("abc", 10).is_none());
    assert!(matcher("-zzz").snippet("abc zzz", 10).is_none());
    assert!(matcher("abc").snippet("abc", 0).is_none());
}

#[test]
fn malformed_query_matches_nothing() {
    let q = Query::parse_lenient(")", &syntax());
    assert!(q.matches_nothing());
    let m = TextMatcher::new(&q).unwrap();
    assert!(!m.is_empty());
    assert!(!m.matches_with(&["anything"], Vec::new));
    assert!(m.snippet("anything", 10).is_none());
}

/// `filters_only` drops mixed OR groups silently, so the caller must build the matcher
/// first: `TextMatcher::new` refuses the mixed group, and only then is `filters_only`
/// safe to use for the operator side.
#[test]
fn filters_only_pairs_with_matcher_built_first() {
    let mixed = parse("from:alice (invoice OR from:bob)");
    assert_eq!(
        TextMatcher::new(&mixed).unwrap_err(),
        MatchError::MixedOrGroup,
        "matcher must refuse the mixed group before filters_only is used"
    );
    // filters_only alone would have kept the operator and dropped the group.
    let filters = mixed.filters_only();
    assert_eq!(filters.ops, mixed.ops);
    assert!(filters.op_or_groups.is_empty());
    assert!(filters.text.is_empty() && filters.or_groups.is_empty());

    // An operator-only OR group is accepted by the matcher and kept by filters_only.
    let ops_only = parse("(from:alice OR label:x) invoice");
    let m = TextMatcher::new(&ops_only).expect("operator-only group is not free text");
    assert!(!m.is_empty());
    assert_eq!(ops_only.filters_only().op_or_groups, ops_only.op_or_groups);
}
