//! Synthetic fixtures on an 80 ms lattice, like Parakeet's timestamps.
//!
//! Each fixture gives the script, the words the ASR "heard" with their true
//! times, and the annotated truth boundaries. ASR times are quantised to the
//! 80 ms lattice with the 80 ms minimum duration, so a passing boundary check
//! shows the lattice error stays inside the 50 ms tolerance.

use rightkit_align::normalise::units;
use rightkit_align::{
    align, asr_words_from_seconds, boundary_errors, Boundary, ControlAnchor, Outcome, RejectReason,
    LATTICE_MS, MIN_WORD_MS, TIMING_TOLERANCE_MS,
};
use rightkit_transcript::{EndSource, Word};

fn lattice_down(ms: u64) -> u64 {
    (ms + LATTICE_MS / 2) / LATTICE_MS * LATTICE_MS
}

/// Quantises an annotated word to the lattice and enforces the minimum duration.
fn lattice_word(text: &str, start_ms: u64, end_ms: u64) -> Word {
    let start = lattice_down(start_ms);
    let end = lattice_down(end_ms).max(start + MIN_WORD_MS);
    Word {
        text: text.into(),
        start_ms: start,
        end_ms: end,
        confidence: None,
        end_source: EndSource::Model,
    }
}

fn asr(words: &[(&str, u64, u64)]) -> Vec<Word> {
    words
        .iter()
        .map(|&(t, s, e)| lattice_word(t, s, e))
        .collect()
}

fn truth(words: &[(&str, u64, u64)], first_token: usize) -> Vec<Boundary> {
    words
        .iter()
        .enumerate()
        .map(|(i, &(_, s, e))| Boundary {
            token_id: first_token + i,
            start_ms: s,
            end_ms: e,
        })
        .collect()
}

fn control(token_id: usize, label: &str) -> ControlAnchor {
    ControlAnchor {
        token_id,
        label: label.into(),
    }
}

/// Fixture A: a clean narration with a control name, a product name, a number
/// with a decimal, a repeated "the", and silences between words.
const SCRIPT_A: &str =
    "Click Export, then open the PostgreSQL settings and set the timeout to 2.5 seconds.";
const TRUTH_A: &[(&str, u64, u64)] = &[
    ("click", 400, 700),
    ("export", 760, 1180),
    ("then", 1300, 1560),
    ("open", 1640, 1900),
    ("the", 1940, 2020),
    ("postgresql", 2060, 2620),
    ("settings", 2660, 3100),
    ("and", 3140, 3260),
    ("set", 3300, 3480),
    ("the", 3520, 3600),
    ("timeout", 3640, 4080),
    ("to", 4120, 4220),
    ("2.5", 4260, 4700),
    ("seconds", 4740, 5200),
];

#[test]
fn clean_narration_accepts_every_token_on_the_lattice_within_tolerance() {
    let words = asr(TRUTH_A);
    let alignment = align(
        SCRIPT_A,
        &words,
        &[control(1, "Export"), control(0, "Click")],
    );

    // Tokens: Click, Export,, then, open, the, PostgreSQL, settings, and, set, the, timeout, to, 2.5, seconds.
    assert_eq!(alignment.anchors.len(), 14);
    for anchor in &alignment.anchors {
        assert!(
            matches!(anchor.outcome, Outcome::Accepted { .. }),
            "token {} {:?} rejected: {:?}",
            anchor.token_id,
            anchor.text,
            anchor.outcome
        );
        assert_eq!(anchor.lexical_confidence, 1.0);
    }

    let errors = boundary_errors(&alignment, &truth(TRUTH_A, 0));
    for error in &errors {
        assert!(
            error.within_tolerance,
            "token {} out of tolerance: {error:?}",
            error.token_id
        );
        assert!(error.start_error_ms.unwrap().unsigned_abs() <= TIMING_TOLERANCE_MS);
        assert!(error.end_error_ms.unwrap().unsigned_abs() <= TIMING_TOLERANCE_MS);
    }
    assert!(alignment.coverage.meets_floor());
    assert_eq!(alignment.coverage.accepted, 2);
}

#[test]
fn numbers_and_decimals_align_across_written_and_spoken_forms() {
    // ASR spells the decimal as digits; the script writes it as digits too.
    let alignment = align(
        "set 2026 and 3 items",
        &asr(&[
            ("set", 0, 200),
            ("2026", 240, 800),
            ("and", 840, 1000),
            ("three", 1040, 1300),
            ("items", 1340, 1700),
        ]),
        &[],
    );
    let span = alignment.accepted_span(1).expect("2026 accepted");
    assert_eq!((span.start_ms, span.end_ms), (240, 800));
    assert!(alignment
        .anchors
        .iter()
        .all(|a| matches!(a.outcome, Outcome::Accepted { .. })));
}

#[test]
fn a_year_read_differently_is_rejected_rather_than_guessed() {
    // Script "2026" normalises to "two thousand twenty six"; the ASR chose "twenty twenty six".
    let alignment = align(
        "released 2026",
        &asr(&[
            ("released", 0, 400),
            ("twenty", 440, 700),
            ("twenty", 740, 900),
            ("six", 940, 1100),
        ]),
        &[control(1, "2026")],
    );
    let anchor = alignment.anchor(1).unwrap();
    assert!(
        matches!(anchor.outcome, Outcome::Rejected { .. }),
        "{:?}",
        anchor.outcome
    );
    assert!(!alignment.coverage.meets_floor());
}

#[test]
fn contractions_match_their_expanded_spoken_form() {
    let alignment = align(
        "Don't click save",
        &asr(&[
            ("do", 0, 200),
            ("not", 240, 400),
            ("click", 440, 700),
            ("save", 740, 1000),
        ]),
        &[],
    );
    assert!(alignment
        .anchors
        .iter()
        .all(|a| matches!(a.outcome, Outcome::Accepted { .. })));
    assert_eq!(alignment.accepted_span(0).unwrap().end_ms, 400);
}

#[test]
fn an_omitted_word_is_rejected_as_unaligned_and_neighbours_still_align() {
    let alignment = align(
        "Click please Save now",
        &asr(&[("click", 0, 300), ("save", 400, 700), ("now", 800, 1000)]),
        &[control(2, "Save")],
    );
    assert!(matches!(
        alignment.anchor(1).unwrap().outcome,
        Outcome::Rejected {
            reason: RejectReason::Unaligned
        }
    ));
    assert!(alignment.accepted_span(0).is_some());
    assert!(alignment.accepted_span(2).is_some());
    assert!(alignment.coverage.meets_floor());
}

#[test]
fn a_misheard_product_name_is_rejected_and_fails_coverage_even_though_neighbours_pass() {
    let alignment = align(
        "Open PostgreSQL now",
        &asr(&[
            ("open", 0, 300),
            ("postgres", 400, 1000),
            ("now", 1100, 1300),
        ]),
        &[control(0, "Open"), control(1, "PostgreSQL")],
    );
    let product = alignment.anchor(1).unwrap();
    assert_eq!(
        product.outcome,
        Outcome::Rejected {
            reason: RejectReason::WordDiffers
        }
    );
    assert_eq!(product.asr_text.as_deref(), Some("postgres"));
    assert!(
        product.lexical_confidence > 0.6 && product.lexical_confidence < 1.0,
        "{}",
        product.lexical_confidence
    );
    assert!(alignment.accepted_span(0).is_some());

    assert!(!alignment.coverage.meets_floor());
    assert_eq!(alignment.coverage.accepted, 1);
    assert_eq!(alignment.coverage.failures.len(), 1);
    assert_eq!(
        alignment.coverage.failures[0].reason,
        Some(RejectReason::WordDiffers)
    );
}

#[test]
fn a_repeated_word_with_two_equal_candidates_is_ambiguous_and_rejected() {
    let alignment = align(
        "go the",
        &asr(&[("go", 0, 300), ("the", 400, 600), ("the", 700, 900)]),
        &[],
    );
    assert!(alignment.accepted_span(0).is_some());
    assert_eq!(
        alignment.anchor(1).unwrap().outcome,
        Outcome::Rejected {
            reason: RejectReason::Ambiguous
        }
    );
}

#[test]
fn a_repeated_word_in_unique_context_is_accepted() {
    let alignment = align(
        "the save the file",
        &asr(&[
            ("the", 0, 200),
            ("save", 240, 500),
            ("the", 540, 700),
            ("file", 740, 1000),
        ]),
        &[],
    );
    assert!(alignment
        .anchors
        .iter()
        .all(|a| matches!(a.outcome, Outcome::Accepted { .. })));
}

#[test]
fn punctuation_only_tokens_are_listed_but_never_accepted() {
    let alignment = align(
        "Save \u{2014} now",
        &asr(&[("save", 0, 300), ("now", 400, 700)]),
        &[],
    );
    assert_eq!(alignment.anchors.len(), 3);
    assert_eq!(
        alignment.anchor(1).unwrap().outcome,
        Outcome::Rejected {
            reason: RejectReason::NoSpokenForm
        }
    );
    assert_eq!(alignment.anchor(1).unwrap().lexical_confidence, 0.0);
}

#[test]
fn designated_control_names_must_all_be_accepted() {
    let words = asr(TRUTH_A);
    let ok = align(
        SCRIPT_A,
        &words,
        &[
            control(1, "Export"),
            control(0, "Click"),
            control(5, "PostgreSQL"),
        ],
    );
    assert!(ok.coverage.meets_floor());
    assert_eq!(ok.coverage.designated, 3);
    assert_eq!(ok.coverage.accepted, 3);

    // A designation past the end of the script is a failure with no reason.
    let missing = align(SCRIPT_A, &words, &[control(99, "Ghost")]);
    assert!(!missing.coverage.meets_floor());
    assert_eq!(missing.coverage.failures[0].reason, None);
}

#[test]
fn timing_confidence_reflects_how_the_asr_derived_the_boundaries() {
    let words = vec![
        Word {
            text: "click".into(),
            start_ms: 0,
            end_ms: 320,
            confidence: Some(0.9),
            end_source: EndSource::Model,
        },
        Word {
            text: "save".into(),
            start_ms: 400,
            end_ms: 720,
            confidence: Some(0.7),
            end_source: EndSource::Interpolated,
        },
    ];
    let alignment = align("click save", &words, &[]);
    let click = alignment.anchor(0).unwrap();
    let save = alignment.anchor(1).unwrap();
    assert_eq!(click.timing_confidence, 1.0);
    assert!((save.timing_confidence - 0.3).abs() < 1e-6);
    assert_eq!(click.asr_confidence, Some(0.9));
    assert_eq!(save.asr_confidence, Some(0.7));
}

#[test]
fn a_boundary_error_beyond_tolerance_is_reported_without_rejecting_the_anchor() {
    let words = asr(&[("click", 400, 700)]);
    let alignment = align("click", &words, &[]);
    // Annotated truth says the word starts 120 ms earlier than the ASR reports.
    let errors = boundary_errors(
        &alignment,
        &[Boundary {
            token_id: 0,
            start_ms: 280,
            end_ms: 700,
        }],
    );
    assert!(matches!(
        alignment.anchor(0).unwrap().outcome,
        Outcome::Accepted { .. }
    ));
    assert_eq!(errors[0].start_error_ms, Some(120));
    assert!(!errors[0].within_tolerance);
}

#[test]
fn a_rejected_anchor_never_counts_as_within_tolerance() {
    let alignment = align("please click", &asr(&[("click", 0, 300)]), &[]);
    let errors = boundary_errors(
        &alignment,
        &[Boundary {
            token_id: 0,
            start_ms: 0,
            end_ms: 300,
        }],
    );
    assert_eq!(errors[0].start_error_ms, None);
    assert!(!errors[0].within_tolerance);
}

#[test]
fn asr_seconds_adapter_produces_model_timed_words_on_the_lattice() {
    let words = asr_words_from_seconds([("save", 0.4f32, 0.72f32), ("now", 0.8, 1.04)]);
    assert_eq!(words[0].start_ms, 400);
    assert_eq!(words[0].end_source, EndSource::Model);
    assert_eq!(words[1].end_ms, 1040);
    assert_eq!(LATTICE_MS, 80);
}

#[test]
fn normalisation_is_shared_by_script_and_asr_sides() {
    assert_eq!(units("2026"), units("two thousand twenty six"));
    assert_eq!(units("don't"), units("do not"));
}
