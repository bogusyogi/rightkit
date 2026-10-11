//! Unicode default case folding: pairs that `str::to_lowercase` did not match.

use rightkit_search::fold::{check_fold_version, fold, fold_with, FoldOptions, FOLD_VERSION};
use rightkit_search::{Query, Syntax, TextMatcher};

fn matcher(input: &str, opts: FoldOptions) -> TextMatcher {
    let q = Query::parse(input, &Syntax::new(["from"])).expect("query parses");
    TextMatcher::with_options(&q, opts).expect("plain text compiles")
}

fn hit(needle: &str, haystack: &str) -> bool {
    matcher(needle, FoldOptions::default()).matches_with(&[haystack], Vec::new)
}

#[test]
fn sharp_s_matches_ss_both_ways() {
    assert_eq!(fold("Straße"), "strasse");
    assert!(hit("strasse", "Hauptstraße 5"));
    assert!(hit("STRAßE", "STRASSE"));
    assert!(hit("ẞ", "ss"));
}

#[test]
fn final_sigma_matches_sigma() {
    assert_eq!(fold("ΟΔΥΣΣΕΥΣ"), fold("οδυσσευς"));
    assert_eq!(fold("οδυσσευς"), fold("οδυσσευσ"));
    assert!(hit("οδυσσεύσ", "ΟΔΥΣΣΕΎΣ"));
    assert!(hit("οδυσσεύς", "οδυσσεύσ"));
}

#[test]
fn long_s_micro_and_greek_variants() {
    assert!(hit("s", "ſ"));
    assert_eq!(fold("\u{00B5}"), fold("\u{03BC}"));
    assert_eq!(
        fold("\u{3D0}\u{3D1}\u{3D5}\u{3D6}\u{3F0}\u{3F1}\u{3F5}"),
        "\u{3B2}\u{3B8}\u{3C6}\u{3C0}\u{3BA}\u{3C1}\u{3B5}"
    );
}

#[test]
fn ligatures_expand() {
    assert!(hit("office", "o\u{FB03}ce"));
    assert!(hit("fish", "\u{FB01}sh"));
}

#[test]
fn default_is_locale_neutral_for_dotted_and_dotless_i() {
    // Locale-neutral: I -> i, dotted capital I folds to i + combining dot, dotless i stays.
    assert_eq!(fold("I"), "i");
    assert_eq!(fold("\u{0130}"), "i\u{0307}");
    assert_eq!(fold("\u{0131}"), "\u{0131}");
    assert!(!hit("ısparta", "ISPARTA"));
    assert!(hit("\u{0130}stanbul", "\u{0130}STANBUL"));
    assert!(!hit("istanbul", "\u{0130}stanbul"));
}

#[test]
fn turkic_option_folds_i_pairs() {
    let t = FoldOptions::turkic();
    assert_eq!(fold_with("ISPARTA", t), fold_with("ısparta", t));
    assert_eq!(fold_with("\u{0130}stanbul", t), fold_with("istanbul", t));
    assert_ne!(fold_with("I", t), fold_with("i", t));
    assert!(matcher("ısparta", t).matches_with(&["ISPARTA"], Vec::new));
    assert!(matcher("istanbul", t).matches_with(&["\u{0130}stanbul"], Vec::new));
}

#[test]
fn existing_behaviour_is_kept() {
    assert_eq!(fold("ÉCOLE"), "école");
    assert!(hit("Café", "CAFÉ"));
    // No normalisation: decomposed does not match precomposed.
    assert!(!hit("e\u{0301}", "é"));
}

#[test]
fn snippet_offsets_survive_expansion() {
    let m = matcher("strasse", FoldOptions::default());
    let s = m.snippet("Die Hauptstraße ist lang", 40).expect("hit");
    let matched: Vec<&str> = s
        .spans
        .iter()
        .filter(|sp| sp.matched)
        .map(|sp| sp.text.as_str())
        .collect();
    assert_eq!(matched, vec!["straße"]);
}

#[test]
fn fold_version_mismatch_is_a_typed_rebuild() {
    assert!(check_fold_version(FOLD_VERSION).is_ok());
    let err = check_fold_version(1).unwrap_err();
    assert_eq!((err.stored, err.current), (1, FOLD_VERSION));
}
