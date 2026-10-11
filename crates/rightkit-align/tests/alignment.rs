//! Synthetic fixtures on an 80 ms lattice, like Parakeet's timestamps.
//!
//! Each fixture gives the script, the words the ASR "heard" with their true
//! times, and the annotated truth boundaries. ASR times are quantised to the
//! 80 ms lattice with the 80 ms minimum duration, so a passing boundary check
//! shows the lattice error stays inside the 50 ms tolerance.
//!
//! Real annotated narration audio (RK4 item f) is not here. It waits on CutRight
//! audio. The `real_annotated_audio_fixtures_are_pending` test below records
//! that gap instead of silently passing.

use rightkit_align::normalise::units;
use rightkit_align::{
    align, asr_words_from_seconds, boundary_errors, AlignOptions, Alignment, Boundary,
    DesignatedAnchor, Outcome, RejectReason, ScriptSpec, LATTICE_MS, MIN_WORD_MS,
    TIMING_TOLERANCE_MS,
};
use rightkit_transcript::{EndSource, Word};

/// Sample rate of the synthetic audio these fixtures stand in for.
const RATE: u32 = 16_000;

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

/// Truth boundaries for the tokens `w{first_token}..`, in the order given.
fn truth(words: &[(&str, u64, u64)], first_token: usize) -> Vec<Boundary> {
    words
        .iter()
        .enumerate()
        .map(|(i, &(_, s, e))| Boundary {
            word_id: format!("w{}", first_token + i),
            start_ms: s,
            end_ms: e,
        })
        .collect()
}

fn designated(word_id: &str, target_id: &str) -> DesignatedAnchor {
    DesignatedAnchor {
        word_id: word_id.into(),
        target_id: target_id.into(),
    }
}

/// Aligns text as a single step with the default options at [`RATE`].
fn run(script: &str, words: &[Word], designated: &[DesignatedAnchor]) -> Alignment {
    align(
        &ScriptSpec::from_text("step-1", script),
        words,
        RATE,
        designated,
        &AlignOptions::default(),
    )
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
    let alignment = run(
        SCRIPT_A,
        &words,
        &[designated("w1", "t-export"), designated("w0", "t-click")],
    );

    // Tokens: Click, Export,, then, open, the, PostgreSQL, settings, and, set, the, timeout, to, 2.5, seconds.
    assert_eq!(alignment.anchors.len(), 14);
    for anchor in &alignment.anchors {
        assert!(
            matches!(anchor.outcome, Outcome::Accepted { .. }),
            "{} {:?} rejected: {:?}",
            anchor.word_id,
            anchor.text,
            anchor.outcome
        );
        assert_eq!(anchor.lexical_confidence, 1.0);
        // Model-timed on the 80 ms lattice with a 50 ms tolerance: never a flat 1.0.
        assert!(
            (anchor.timing_confidence - 0.625).abs() < 1e-6,
            "{}: {}",
            anchor.word_id,
            anchor.timing_confidence
        );
    }

    let errors = boundary_errors(&alignment, &truth(TRUTH_A, 0));
    for error in &errors {
        assert!(
            error.within_tolerance,
            "{} out of tolerance: {error:?}",
            error.word_id
        );
        assert!(error.start_error_ms.unwrap().unsigned_abs() <= TIMING_TOLERANCE_MS);
        assert!(error.end_error_ms.unwrap().unsigned_abs() <= TIMING_TOLERANCE_MS);
    }
    assert!(alignment.coverage.meets_floor());
    assert_eq!(alignment.coverage.accepted, 2);
    assert_eq!(alignment.coverage.designated, 2);
}

#[test]
fn clean_narration_splits_into_clauses_on_punctuation_with_timed_endpoints() {
    let alignment = run(SCRIPT_A, &asr(TRUTH_A), &[]);

    assert_eq!(alignment.clauses.len(), 2, "{:?}", alignment.clauses);
    let first = &alignment.clauses[0];
    assert_eq!(first.clause_id, "c0");
    assert_eq!(first.step_id, "step-1");
    assert_eq!(first.first_word_id, "w0");
    assert_eq!(first.last_word_id, "w1");
    assert_eq!(first.start.start_ms, 400);

    let second = &alignment.clauses[1];
    assert_eq!(second.clause_id, "c1");
    assert_eq!(second.first_word_id, "w2");
    assert_eq!(second.last_word_id, "w13");
    assert_eq!(second.end.end_ms, 5200);
    assert_eq!(second.end.sample_rate, RATE);
}

#[test]
fn numbers_and_decimals_align_across_written_and_spoken_forms() {
    // ASR spells the number as a word; the script writes it as digits.
    let alignment = run(
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
    let span = alignment.accepted_span("w1").expect("2026 accepted");
    assert_eq!((span.start_ms, span.end_ms), (240, 800));
    assert!(alignment
        .anchors
        .iter()
        .all(|a| matches!(a.outcome, Outcome::Accepted { .. })));
}

#[test]
fn accepted_spans_carry_samples_at_the_aligner_sample_rate() {
    let alignment = align(
        &ScriptSpec::from_text("s", "save now"),
        &asr(&[("save", 400, 720), ("now", 800, 1040)]),
        48_000,
        &[],
        &AlignOptions::default(),
    );
    let span = alignment.accepted_span("w0").unwrap();
    assert_eq!(span.sample_rate, 48_000);
    assert_eq!((span.start_ms, span.end_ms), (400, 720));
    assert_eq!((span.start_sample, span.end_sample), (19_200, 34_560));
    assert_eq!(alignment.clauses[0].start.start_sample, 19_200);
}

#[test]
fn a_year_read_differently_is_rejected_rather_than_guessed() {
    // Script "2026" normalises to "two thousand twenty six"; the ASR chose "twenty twenty six".
    let alignment = run(
        "released 2026",
        &asr(&[
            ("released", 0, 400),
            ("twenty", 440, 700),
            ("twenty", 740, 900),
            ("six", 940, 1100),
        ]),
        &[designated("w1", "t-year")],
    );
    let anchor = alignment.anchor("w1").unwrap();
    assert!(
        matches!(anchor.outcome, Outcome::Rejected { .. }),
        "{:?}",
        anchor.outcome
    );
    assert!(!alignment.coverage.meets_floor());
}

#[test]
fn contractions_match_their_expanded_spoken_form() {
    let alignment = run(
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
    assert_eq!(alignment.accepted_span("w0").unwrap().end_ms, 400);
}

#[test]
fn an_omitted_word_is_rejected_as_unaligned_and_neighbours_still_align() {
    let alignment = run(
        "Click please Save now",
        &asr(&[("click", 0, 300), ("save", 400, 700), ("now", 800, 1000)]),
        &[designated("w2", "t-save")],
    );
    assert!(matches!(
        alignment.anchor("w1").unwrap().outcome,
        Outcome::Rejected {
            reason: RejectReason::Unaligned
        }
    ));
    assert!(alignment.accepted_span("w0").is_some());
    assert!(alignment.accepted_span("w2").is_some());
    assert!(alignment.coverage.meets_floor());
}

#[test]
fn a_misheard_product_name_is_rejected_and_fails_coverage_even_though_neighbours_pass() {
    let alignment = run(
        "Open PostgreSQL now",
        &asr(&[
            ("open", 0, 300),
            ("postgres", 400, 1000),
            ("now", 1100, 1300),
        ]),
        &[designated("w0", "t-open"), designated("w1", "t-pg")],
    );
    let product = alignment.anchor("w1").unwrap();
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
    // The rejected token keeps its timing trust scaled by the lexical match.
    assert!(
        (product.timing_confidence - 0.625 * product.lexical_confidence).abs() < 1e-6,
        "{}",
        product.timing_confidence
    );
    assert!(alignment.accepted_span("w0").is_some());

    assert!(!alignment.coverage.meets_floor());
    assert_eq!(alignment.coverage.accepted, 1);
    assert_eq!(alignment.coverage.failures.len(), 1);
    let failure = &alignment.coverage.failures[0];
    assert_eq!(failure.word_id, "w1");
    assert_eq!(failure.target_id, "t-pg");
    assert_eq!(failure.reason, Some(RejectReason::WordDiffers));
}

#[test]
fn a_repeated_word_with_two_equal_candidates_is_ambiguous_and_rejected() {
    let alignment = run(
        "go the",
        &asr(&[("go", 0, 300), ("the", 400, 600), ("the", 700, 900)]),
        &[],
    );
    assert!(alignment.accepted_span("w0").is_some());
    assert_eq!(
        alignment.anchor("w1").unwrap().outcome,
        Outcome::Rejected {
            reason: RejectReason::Ambiguous
        }
    );
}

#[test]
fn a_repeated_word_in_unique_context_is_accepted() {
    let alignment = run(
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
    let alignment = run(
        "Save \u{2014} now",
        &asr(&[("save", 0, 300), ("now", 400, 700)]),
        &[],
    );
    assert_eq!(alignment.anchors.len(), 3);
    assert_eq!(
        alignment.anchor("w1").unwrap().outcome,
        Outcome::Rejected {
            reason: RejectReason::NoSpokenForm
        }
    );
    assert_eq!(alignment.anchor("w1").unwrap().lexical_confidence, 0.0);
    // The dash is not a clause endpoint, so "Save — now" is one clause from w0 to w2.
    assert_eq!(alignment.clauses.len(), 1);
    assert_eq!(alignment.clauses[0].last_word_id, "w2");
}

#[test]
fn designated_control_names_must_all_be_accepted_and_rows_echo_target_ids() {
    let words = asr(TRUTH_A);
    let ok = run(
        SCRIPT_A,
        &words,
        &[
            designated("w1", "t-export"),
            designated("w0", "t-click"),
            designated("w5", "t-pg"),
        ],
    );
    assert!(ok.coverage.meets_floor());
    assert_eq!(ok.coverage.designated, 3);
    assert_eq!(ok.coverage.accepted, 3);
    assert_eq!(
        ok.anchor("w1").unwrap().target_id.as_deref(),
        Some("t-export")
    );
    assert_eq!(ok.anchor("w2").unwrap().target_id, None);
    assert_eq!(ok.anchor("w2").unwrap().step_id, "step-1");

    // A designation past the end of the script is a failure with no reason.
    let missing = run(SCRIPT_A, &words, &[designated("w99", "t-ghost")]);
    assert!(!missing.coverage.meets_floor());
    assert_eq!(missing.coverage.failures[0].reason, None);
    assert_eq!(missing.coverage.failures[0].word_id, "w99");
    assert_eq!(missing.coverage.failures[0].target_id, "t-ghost");
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
    let alignment = run("click save", &words, &[]);
    let click = alignment.anchor("w0").unwrap();
    let save = alignment.anchor("w1").unwrap();
    // Parakeet lattice bound: 50 / 80, not a flat 1.0. Interpolated stays 0.3.
    assert!((click.timing_confidence - 0.625).abs() < 1e-6);
    assert!((save.timing_confidence - 0.3).abs() < 1e-6);
    assert_eq!(click.asr_confidence, Some(0.9));
    assert_eq!(save.asr_confidence, Some(0.7));
}

#[test]
fn a_continuous_timing_assertion_is_the_only_way_to_reach_full_trust() {
    let words = asr(&[("click", 400, 700)]);
    let continuous = align(
        &ScriptSpec::from_text("s", "click"),
        &words,
        RATE,
        &[],
        &AlignOptions {
            lattice_ms: None,
            ..AlignOptions::default()
        },
    );
    assert_eq!(continuous.anchors[0].timing_confidence, 1.0);
}

#[test]
fn a_wider_tolerance_than_the_lattice_saturates_at_one() {
    let words = asr(&[("click", 400, 700)]);
    let wide = align(
        &ScriptSpec::from_text("s", "click"),
        &words,
        RATE,
        &[],
        &AlignOptions {
            tolerance_ms: 120,
            ..AlignOptions::default()
        },
    );
    assert_eq!(wide.anchors[0].timing_confidence, 1.0);
}

#[test]
fn a_boundary_error_beyond_tolerance_is_reported_without_rejecting_the_anchor() {
    let words = asr(&[("click", 400, 700)]);
    let alignment = run("click", &words, &[]);
    // Annotated truth says the word starts 120 ms earlier than the ASR reports.
    let errors = boundary_errors(
        &alignment,
        &[Boundary {
            word_id: "w0".into(),
            start_ms: 280,
            end_ms: 700,
        }],
    );
    assert!(matches!(
        alignment.anchor("w0").unwrap().outcome,
        Outcome::Accepted { .. }
    ));
    assert_eq!(errors[0].start_error_ms, Some(120));
    assert!(!errors[0].within_tolerance);
}

#[test]
fn the_tolerance_the_alignment_was_built_with_decides_within_tolerance() {
    let words = asr(&[("click", 400, 700)]);
    let truth = [Boundary {
        word_id: "w0".into(),
        start_ms: 370,
        end_ms: 720,
    }];
    let default = run("click", &words, &[]);
    assert!(boundary_errors(&default, &truth)[0].within_tolerance);

    let strict = align(
        &ScriptSpec::from_text("step-1", "click"),
        &words,
        RATE,
        &[],
        &AlignOptions {
            tolerance_ms: 10,
            ..AlignOptions::default()
        },
    );
    assert!(!boundary_errors(&strict, &truth)[0].within_tolerance);
}

#[test]
fn a_rejected_anchor_never_counts_as_within_tolerance() {
    let alignment = run("please click", &asr(&[("click", 0, 300)]), &[]);
    let errors = boundary_errors(
        &alignment,
        &[Boundary {
            word_id: "w0".into(),
            start_ms: 0,
            end_ms: 300,
        }],
    );
    assert_eq!(errors[0].start_error_ms, None);
    assert!(!errors[0].within_tolerance);
}

#[test]
fn clauses_split_on_step_boundaries_without_punctuation() {
    let mut script = ScriptSpec::from_text("a", "open file");
    let mut second = ScriptSpec::from_text("b", "save now");
    for token in &mut second.tokens {
        token.token_index += 2;
        token.word_id = format!("x{}", token.token_index);
    }
    script.tokens.extend(second.tokens);

    let alignment = align(
        &script,
        &asr(&[
            ("open", 0, 300),
            ("file", 400, 700),
            ("save", 800, 1100),
            ("now", 1200, 1400),
        ]),
        RATE,
        &[],
        &AlignOptions::default(),
    );
    assert_eq!(alignment.clauses.len(), 2);
    assert_eq!(alignment.clauses[0].step_id, "a");
    assert_eq!(alignment.clauses[0].last_word_id, "w1");
    assert_eq!(alignment.clauses[1].step_id, "b");
    assert_eq!(alignment.clauses[1].first_word_id, "x2");
    assert_eq!(alignment.clauses[1].last_word_id, "x3");
}

#[test]
fn a_newline_ends_a_clause() {
    let alignment = run(
        "Save\nnow later",
        &asr(&[("save", 0, 300), ("now", 400, 700), ("later", 800, 1100)]),
        &[],
    );
    assert_eq!(alignment.clauses.len(), 2);
    assert_eq!(alignment.clauses[0].last_word_id, "w0");
    assert_eq!(alignment.clauses[1].first_word_id, "w1");
    assert_eq!(alignment.clauses[1].last_word_id, "w2");
}

#[test]
fn a_clause_is_dropped_when_its_last_spoken_token_is_not_accepted() {
    let alignment = run("Save now.", &asr(&[("save", 0, 300)]), &[]);
    assert!(alignment.clauses.is_empty());
    assert!(matches!(
        alignment.anchor("w1").unwrap().outcome,
        Outcome::Rejected {
            reason: RejectReason::Unaligned
        }
    ));
}

#[test]
fn a_rejected_word_inside_a_clause_keeps_the_clause_when_its_endpoints_pass() {
    let alignment = run(
        "Click please Save now",
        &asr(&[("click", 0, 300), ("save", 400, 700), ("now", 800, 1000)]),
        &[],
    );
    assert_eq!(alignment.clauses.len(), 1);
    assert_eq!(alignment.clauses[0].first_word_id, "w0");
    assert_eq!(alignment.clauses[0].last_word_id, "w3");
}

#[test]
#[should_panic(expected = "duplicate word_id")]
fn duplicate_word_ids_are_refused_rather_than_aligned() {
    let mut script = ScriptSpec::from_text("s", "go the");
    script.tokens[1].word_id = "w0".into();
    align(
        &script,
        &asr(&[("go", 0, 300)]),
        RATE,
        &[],
        &AlignOptions::default(),
    );
}

#[test]
#[should_panic(expected = "asr_sample_rate")]
fn a_zero_sample_rate_is_refused() {
    align(
        &ScriptSpec::from_text("s", "go"),
        &asr(&[("go", 0, 300)]),
        0,
        &[],
        &AlignOptions::default(),
    );
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

#[test]
fn real_annotated_audio_fixtures_are_pending() {
    // RK4 item f: fixtures on real annotated narration audio. They wait on
    // CutRight audio, so this test is a documented skip rather than a pass.
    eprintln!("SKIPPED: real annotated audio fixtures (RK4 f) await CutRight audio");
}
