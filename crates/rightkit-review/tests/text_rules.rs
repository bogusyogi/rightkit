//! Each text rule passes at its limit, fails over it, and reports inconclusive
//! when its inputs cannot support a verdict. Unicode, empty text and malformed
//! policy data are covered. Judges never change a text disposition.

mod support;

use rightkit_review::{
    disposition, run_repair_loop, ApprovedProof, ClaimAnnotation, Disposition, Finding, Judge,
    JudgeScore, NoJudge, RepairCap, ReviewError, Rule, RuleKind, RuleSet, StopReason, Subject,
    Verdict,
};
use support::{clean_video, text_subject, text_subject_with};

fn finding(id: &str, kind: RuleKind, subject: &Subject) -> Finding {
    let set = RuleSet::new(
        "test.text",
        1,
        vec![Rule {
            id: id.to_string(),
            required: true,
            kind,
        }],
    )
    .expect("valid rule set");
    let mut findings = set.evaluate(subject).expect("valid subject");
    assert_eq!(findings.len(), 1);
    findings.remove(0)
}

/// Short status for one verdict, so assertions read as pass, fail or inconclusive.
fn status(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Verified => "pass",
        Verdict::Failed { .. } => "fail",
        Verdict::Inconclusive { .. } => "inconclusive",
        Verdict::NotApplicable { .. } => "not_applicable",
        Verdict::Unverified { .. } => "unverified",
        Verdict::Held { .. } => "held",
    }
}

fn check(id: &str, kind: RuleKind, body: &str) -> Verdict {
    finding(id, kind, &text_subject(body)).verdict
}

fn phrases(list: &[&str]) -> Vec<String> {
    list.iter().map(|phrase| phrase.to_string()).collect()
}

/// A body of exactly one hundred words, followed by `tail`.
fn hundred_words_then(tail: &str) -> String {
    format!("{}{tail}", "word ".repeat(97))
}

fn claim(value: &str, unit: &str, subject: &str, date: &str) -> ClaimAnnotation {
    ClaimAnnotation {
        value: value.to_string(),
        unit: unit.to_string(),
        subject: subject.to_string(),
        date: date.to_string(),
    }
}

fn reach_proof() -> ApprovedProof {
    ApprovedProof {
        id: "p-1".to_string(),
        value: "12.5".to_string(),
        unit: "%".to_string(),
        subject: "reach".to_string(),
        valid_from: "2026-01-01".to_string(),
        valid_until: "2026-12-31".to_string(),
    }
}

fn claim_kind(proofs: Vec<ApprovedProof>) -> RuleKind {
    RuleKind::ClaimProof {
        approved_proofs: proofs,
        min_words: 1,
    }
}

fn claim_status(value: &str, unit: &str, subject: &str, date: &str) -> &'static str {
    let body = text_subject_with(
        "Reach grew by twelve point five percent.",
        vec![claim(value, unit, subject, date)],
    );
    status(
        &finding(
            "det.text.claim_proof",
            claim_kind(vec![reach_proof()]),
            &body,
        )
        .verdict,
    )
}

// det.text.banned_phrase

#[test]
fn banned_phrase_clean_text_passes() {
    let kind = RuleKind::BannedPhrase {
        phrases: phrases(&["cure"]),
        min_words: 3,
    };
    let verdict = check(
        "det.text.banned_phrase",
        kind,
        "The procurement team met the vendor on Tuesday.",
    );
    assert_eq!(status(&verdict), "pass");
}

#[test]
fn banned_phrase_matches_case_insensitively_and_fails() {
    let kind = RuleKind::BannedPhrase {
        phrases: phrases(&["in conclusion"]),
        min_words: 1,
    };
    let found = finding(
        "det.text.banned_phrase",
        kind,
        &text_subject("IN CONCLUSION, we shipped the release."),
    );
    assert_eq!(found.rule_id, "det.text.banned_phrase");
    assert_eq!(status(&found.verdict), "fail");
}

#[test]
fn banned_phrase_with_hyphen_and_typographic_apostrophe_fails() {
    let hyphen = RuleKind::BannedPhrase {
        phrases: phrases(&["game-changer"]),
        min_words: 1,
    };
    assert_eq!(
        status(&check(
            "det.text.banned_phrase",
            hyphen,
            "This is a Game-Changer."
        )),
        "fail"
    );
    let apostrophe = RuleKind::BannedPhrase {
        phrases: phrases(&["don't"]),
        min_words: 1,
    };
    assert_eq!(
        status(&check(
            "det.text.banned_phrase",
            apostrophe,
            "We don\u{2019}t cut corners."
        )),
        "fail"
    );
}

#[test]
fn banned_phrase_respects_whole_word_boundaries_for_unicode() {
    let kind = RuleKind::BannedPhrase {
        phrases: phrases(&["naïve"]),
        min_words: 1,
    };
    assert_eq!(
        status(&check(
            "det.text.banned_phrase",
            kind.clone(),
            "A naïvely written note here."
        )),
        "pass",
        "a longer word that contains the phrase is not a whole-word match"
    );
    assert_eq!(
        status(&check("det.text.banned_phrase", kind, "A NAÏVE note here.")),
        "fail"
    );
}

#[test]
fn banned_phrase_folds_accented_case() {
    let kind = RuleKind::BannedPhrase {
        phrases: phrases(&["café"]),
        min_words: 1,
    };
    assert_eq!(
        status(&check(
            "det.text.banned_phrase",
            kind,
            "Visit the CAFÉ on Monday."
        )),
        "fail"
    );
}

#[test]
fn banned_phrase_matches_at_text_edges_next_to_punctuation() {
    let kind = RuleKind::BannedPhrase {
        phrases: phrases(&["cure"]),
        min_words: 1,
    };
    assert_eq!(
        status(&check("det.text.banned_phrase", kind, "Cure!")),
        "fail"
    );
}

#[test]
fn banned_phrase_empty_text_passes_with_no_minimum_and_is_inconclusive_below_one() {
    let lenient = RuleKind::BannedPhrase {
        phrases: phrases(&["cure"]),
        min_words: 0,
    };
    assert_eq!(
        status(&check("det.text.banned_phrase", lenient, "")),
        "pass"
    );
    let strict = RuleKind::BannedPhrase {
        phrases: phrases(&["cure"]),
        min_words: 1,
    };
    assert_eq!(
        status(&check("det.text.banned_phrase", strict, "")),
        "inconclusive"
    );
}

#[test]
fn banned_phrase_below_minimum_word_count_is_inconclusive() {
    let kind = RuleKind::BannedPhrase {
        phrases: phrases(&["cure"]),
        min_words: 3,
    };
    assert_eq!(
        status(&check("det.text.banned_phrase", kind, "Cure.")),
        "inconclusive"
    );
}

// det.text.dash_density

fn dashes(max: f64, min_words: u32) -> RuleKind {
    RuleKind::DashDensity {
        max_per_100_words: max,
        min_words,
    }
}

#[test]
fn dash_density_at_limit_passes_and_over_limit_fails() {
    // 100 words with 2 dashes is exactly 2.0 per 100 words.
    let at_limit = hundred_words_then("a\u{2014}b\u{2013}c");
    assert_eq!(
        status(&check("det.text.dash_density", dashes(2.0, 1), &at_limit)),
        "pass"
    );
    // 100 words with 3 dashes is 3.0 per 100 words.
    let over = hundred_words_then("a\u{2014}b\u{2013}c\u{2014}");
    assert_eq!(
        status(&check("det.text.dash_density", dashes(2.0, 1), &over)),
        "fail"
    );
}

#[test]
fn dash_density_unicode_text_is_measured_on_words() {
    // Three words and two dashes: 66.67 per 100 words, under a limit of 100.
    let body = "Café \u{2014} naïve \u{2013} résumé";
    assert_eq!(
        status(&check("det.text.dash_density", dashes(100.0, 1), body)),
        "pass"
    );
    assert_eq!(
        status(&check("det.text.dash_density", dashes(50.0, 1), body)),
        "fail"
    );
}

#[test]
fn dash_density_empty_text_is_inconclusive() {
    assert_eq!(
        status(&check("det.text.dash_density", dashes(2.0, 0), "")),
        "inconclusive"
    );
}

// det.text.semicolon_density

#[test]
fn semicolon_density_at_limit_passes_and_over_limit_fails() {
    let kind = |max| RuleKind::SemicolonDensity {
        max_per_100_words: max,
        min_words: 1,
    };
    let at_limit = hundred_words_then("a;b c");
    assert_eq!(
        status(&check("det.text.semicolon_density", kind(1.0), &at_limit)),
        "pass"
    );
    let over = hundred_words_then("a;b;c");
    assert_eq!(
        status(&check("det.text.semicolon_density", kind(1.0), &over)),
        "fail"
    );
}

#[test]
fn semicolon_density_empty_text_is_inconclusive() {
    let kind = RuleKind::SemicolonDensity {
        max_per_100_words: 1.0,
        min_words: 0,
    };
    assert_eq!(
        status(&check("det.text.semicolon_density", kind, "")),
        "inconclusive"
    );
}

// det.text.sentence_length_variance

fn variance(floor: f64, min_sentences: u32) -> RuleKind {
    RuleKind::SentenceLengthVariance {
        min_variance: floor,
        min_sentences,
        min_words: 1,
    }
}

#[test]
fn sentence_variance_at_floor_passes_and_just_above_fails() {
    // Word counts 2 and 4: mean 3, population variance exactly 1.0.
    let body = "One two. Three four five six.";
    assert_eq!(
        status(&check(
            "det.text.sentence_length_variance",
            variance(1.0, 2),
            body
        )),
        "pass"
    );
    assert_eq!(
        status(&check(
            "det.text.sentence_length_variance",
            variance(1.01, 2),
            body
        )),
        "fail"
    );
}

#[test]
fn sentence_variance_uniform_script_fails() {
    let body = "One two. Three four. Five six.";
    assert_eq!(
        status(&check(
            "det.text.sentence_length_variance",
            variance(1.0, 2),
            body
        )),
        "fail"
    );
}

#[test]
fn sentence_variance_unicode_text_is_measured() {
    // Word counts 2 and 7: variance 6.25.
    let body = "Él vino. Ella fue a la casa grande.";
    assert_eq!(
        status(&check(
            "det.text.sentence_length_variance",
            variance(1.0, 2),
            body
        )),
        "pass"
    );
}

#[test]
fn sentence_variance_too_few_sentences_or_empty_text_is_inconclusive() {
    let body = "One two. Three four five six.";
    assert_eq!(
        status(&check(
            "det.text.sentence_length_variance",
            variance(1.0, 3),
            body
        )),
        "inconclusive"
    );
    assert_eq!(
        status(&check(
            "det.text.sentence_length_variance",
            variance(1.0, 0),
            ""
        )),
        "inconclusive"
    );
}

// det.text.tricolon_frequency

fn tricolons(max: f64) -> RuleKind {
    RuleKind::TricolonFrequency {
        max_per_100_sentences: max,
        min_sentences: 1,
        min_words: 1,
    }
}

#[test]
fn tricolon_at_limit_passes_and_over_limit_fails() {
    // Ten sentences, one tricolon: exactly 10.0 per 100 sentences.
    let at_limit = format!("{}We build, test and ship.", "We ship it well. ".repeat(9));
    assert_eq!(
        status(&check(
            "det.text.tricolon_frequency",
            tricolons(10.0),
            &at_limit
        )),
        "pass"
    );
    // Ten sentences, two tricolons: 20.0 per 100 sentences.
    let over = format!(
        "We build, test and ship. We plan, write or edit. {}",
        "We ship it well. ".repeat(8)
    );
    assert_eq!(
        status(&check(
            "det.text.tricolon_frequency",
            tricolons(10.0),
            &over
        )),
        "fail"
    );
}

#[test]
fn tricolon_needs_comma_space_pattern_and_case_insensitive_conjunction() {
    assert_eq!(
        status(&check(
            "det.text.tricolon_frequency",
            tricolons(0.0),
            "We build fast and ship."
        )),
        "pass",
        "no comma-separated first item, so no tricolon"
    );
    assert_eq!(
        status(&check(
            "det.text.tricolon_frequency",
            tricolons(0.0),
            "Red, green AND blue."
        )),
        "fail"
    );
}

#[test]
fn tricolon_empty_or_short_text_is_inconclusive() {
    assert_eq!(
        status(&check("det.text.tricolon_frequency", tricolons(10.0), "")),
        "inconclusive"
    );
    let needs_two = RuleKind::TricolonFrequency {
        max_per_100_sentences: 10.0,
        min_sentences: 2,
        min_words: 1,
    };
    assert_eq!(
        status(&check(
            "det.text.tricolon_frequency",
            needs_two,
            "We ship it well."
        )),
        "inconclusive"
    );
}

// det.text.stock_opener

fn openers(list: &[&str]) -> RuleKind {
    RuleKind::StockOpener {
        openers: phrases(list),
        min_words: 1,
    }
}

#[test]
fn stock_opener_clean_text_passes_and_mid_sentence_use_is_allowed() {
    let kind = openers(&["in today's", "here's the thing"]);
    assert_eq!(
        status(&check(
            "det.text.stock_opener",
            kind.clone(),
            "We shipped the release. Results follow."
        )),
        "pass"
    );
    assert_eq!(
        status(&check(
            "det.text.stock_opener",
            kind,
            "We said in today's market speed wins."
        )),
        "pass"
    );
}

#[test]
fn stock_opener_matches_opening_words_and_fails() {
    let kind = openers(&["in today's", "here's the thing"]);
    assert_eq!(
        status(&check(
            "det.text.stock_opener",
            kind.clone(),
            "In today's market, speed wins. We shipped."
        )),
        "fail"
    );
    assert_eq!(
        status(&check(
            "det.text.stock_opener",
            kind,
            "Here\u{2019}s the thing: we ship."
        )),
        "fail",
        "curly apostrophe folds to ASCII before matching"
    );
}

#[test]
fn stock_opener_ends_a_sentence_at_a_line_break() {
    let kind = openers(&["in today's"]);
    assert_eq!(
        status(&check(
            "det.text.stock_opener",
            kind,
            "Intro line\nIn today's market\nSpeed wins"
        )),
        "fail"
    );
}

#[test]
fn stock_opener_unicode_and_empty_text() {
    let kind = openers(&["à bientôt"]);
    assert_eq!(
        status(&check(
            "det.text.stock_opener",
            kind,
            "À bientôt, friends. Next."
        )),
        "fail"
    );
    let lenient = RuleKind::StockOpener {
        openers: phrases(&["in today's"]),
        min_words: 0,
    };
    assert_eq!(status(&check("det.text.stock_opener", lenient, "")), "pass");
}

// det.text.claim_proof

#[test]
fn claim_matching_an_approved_proof_passes() {
    assert_eq!(claim_status("12.50", "%", "reach", "2026-10-11"), "pass");
}

#[test]
fn claim_window_bounds_are_inclusive() {
    assert_eq!(claim_status("12.5", "%", "reach", "2026-01-01"), "pass");
    assert_eq!(claim_status("12.5", "%", "reach", "2026-12-31"), "pass");
}

#[test]
fn claim_outside_the_window_fails() {
    assert_eq!(claim_status("12.5", "%", "reach", "2027-01-01"), "fail");
    assert_eq!(claim_status("12.5", "%", "reach", "2025-12-31"), "fail");
}

#[test]
fn claim_value_is_compared_as_an_exact_decimal_string() {
    assert_eq!(claim_status("12.51", "%", "reach", "2026-10-11"), "fail");
    // Exactly equal as a float, but not as a decimal string.
    assert_eq!(
        claim_status("12.50000000000000001", "%", "reach", "2026-10-11"),
        "fail"
    );
    assert_eq!(claim_status("12,5", "%", "reach", "2026-10-11"), "fail");
}

#[test]
fn claim_unit_and_subject_must_match_exactly() {
    assert_eq!(claim_status("12.5", "pp", "reach", "2026-10-11"), "fail");
    assert_eq!(claim_status("12.5", "%", "Reach", "2026-10-11"), "fail");
}

#[test]
fn claim_trailing_zeros_and_leading_zeros_do_not_change_the_value() {
    let proofs = vec![ApprovedProof {
        id: "p-2".to_string(),
        value: "1240".to_string(),
        unit: "users".to_string(),
        subject: "signups".to_string(),
        valid_from: "2026-01-01".to_string(),
        valid_until: "2026-12-31".to_string(),
    }];
    for value in ["1240.00", "001240", "1240.000"] {
        let body = text_subject_with(
            "Signups reached 1,240 users.",
            vec![claim(value, "users", "signups", "2026-06-01")],
        );
        let found = finding("det.text.claim_proof", claim_kind(proofs.clone()), &body);
        assert_eq!(status(&found.verdict), "pass", "value {value}");
    }
    let negative_zero = vec![ApprovedProof {
        value: "0".to_string(),
        ..proofs[0].clone()
    }];
    let body = text_subject_with(
        "Net change was zero.",
        vec![claim("-0.0", "users", "signups", "2026-06-01")],
    );
    let found = finding("det.text.claim_proof", claim_kind(negative_zero), &body);
    assert_eq!(status(&found.verdict), "pass");
}

#[test]
fn claim_with_malformed_date_fails() {
    assert_eq!(claim_status("12.5", "%", "reach", "2026-02-30"), "fail");
    assert_eq!(claim_status("12.5", "%", "reach", "10/11/2026"), "fail");
}

#[test]
fn claim_unicode_subject_matches() {
    let proofs = vec![ApprovedProof {
        subject: "réach".to_string(),
        ..reach_proof()
    }];
    let body = text_subject_with(
        "Réach grew.",
        vec![claim("12.5", "%", "réach", "2026-06-01")],
    );
    let found = finding("det.text.claim_proof", claim_kind(proofs), &body);
    assert_eq!(status(&found.verdict), "pass");
}

#[test]
fn claim_text_without_claims_passes_and_empty_text_is_inconclusive_below_minimum() {
    let kind = RuleKind::ClaimProof {
        approved_proofs: vec![reach_proof()],
        min_words: 0,
    };
    assert_eq!(
        status(&check("det.text.claim_proof", kind, "No numbers here.")),
        "pass"
    );
    let strict = claim_kind(vec![reach_proof()]);
    assert_eq!(
        status(&check("det.text.claim_proof", strict, "")),
        "inconclusive"
    );
}

// Missing text, wrong artifact kind and malformed policy.

#[test]
fn missing_text_body_is_inconclusive_and_blocks_when_required() {
    let mut subject = text_subject("Some words here.");
    subject.artifact.text = None;
    let found = finding(
        "det.text.banned_phrase",
        RuleKind::BannedPhrase {
            phrases: phrases(&["cure"]),
            min_words: 0,
        },
        &subject,
    );
    assert_eq!(status(&found.verdict), "inconclusive");
    assert_eq!(disposition(&[found]), Disposition::Block);
}

#[test]
fn text_rules_are_not_applicable_to_media_subjects() {
    let found = finding("det.text.dash_density", dashes(2.0, 1), &clean_video());
    assert_eq!(status(&found.verdict), "not_applicable");
}

#[test]
fn rule_set_refuses_malformed_text_policy() {
    let refused = |kind: RuleKind| {
        matches!(
            RuleSet::new(
                "test.text",
                1,
                vec![Rule {
                    id: "det.text.x".to_string(),
                    required: true,
                    kind,
                }],
            ),
            Err(ReviewError::InvalidRuleSet(_))
        )
    };
    assert!(refused(RuleKind::BannedPhrase {
        phrases: Vec::new(),
        min_words: 0,
    }));
    assert!(refused(RuleKind::BannedPhrase {
        phrases: phrases(&["   "]),
        min_words: 0,
    }));
    assert!(refused(dashes(f64::NAN, 0)));
    assert!(refused(dashes(-1.0, 0)));
    assert!(refused(RuleKind::SemicolonDensity {
        max_per_100_words: f64::INFINITY,
        min_words: 0,
    }));
    assert!(refused(variance(-0.5, 0)));
    assert!(refused(RuleKind::TricolonFrequency {
        max_per_100_sentences: -2.0,
        min_sentences: 0,
        min_words: 0,
    }));
    assert!(refused(RuleKind::StockOpener {
        openers: Vec::new(),
        min_words: 0,
    }));
    assert!(refused(RuleKind::StockOpener {
        openers: phrases(&["one two three four"]),
        min_words: 0,
    }));
    assert!(refused(RuleKind::ClaimProof {
        approved_proofs: vec![ApprovedProof {
            valid_from: "2026-13-01".to_string(),
            ..reach_proof()
        }],
        min_words: 0,
    }));
    assert!(refused(RuleKind::ClaimProof {
        approved_proofs: vec![ApprovedProof {
            valid_from: "2026-12-31".to_string(),
            valid_until: "2026-01-01".to_string(),
            ..reach_proof()
        }],
        min_words: 0,
    }));
    assert!(refused(RuleKind::ClaimProof {
        approved_proofs: vec![ApprovedProof {
            value: "1.2.3".to_string(),
            ..reach_proof()
        }],
        min_words: 0,
    }));
}

// Judge and disposition: deterministic text rules decide, the judge only records.

struct Perfect;

impl Judge for Perfect {
    fn id(&self) -> &str {
        "perfect-test-judge"
    }
    fn score(&self, _subject: &Subject) -> Result<Vec<JudgeScore>, String> {
        Ok(vec![JudgeScore {
            dimension: "taste".into(),
            score: 1.0,
            rationale: "great".into(),
        }])
    }
}

struct Unavailable;

impl Judge for Unavailable {
    fn id(&self) -> &str {
        "unavailable-test-judge"
    }
    fn score(&self, _subject: &Subject) -> Result<Vec<JudgeScore>, String> {
        Err("model unavailable".into())
    }
}

fn no_repairs(_: &Subject, _: &[Finding], _: u32) -> Option<Subject> {
    None
}

#[test]
fn judge_cannot_pass_a_text_with_a_failed_deterministic_rule() {
    let rules = RuleSet::new(
        "test.text",
        1,
        vec![Rule {
            id: "det.text.banned_phrase".to_string(),
            required: true,
            kind: RuleKind::BannedPhrase {
                phrases: phrases(&["in conclusion"]),
                min_words: 1,
            },
        }],
    )
    .unwrap();
    let mut repairer = no_repairs;
    let outcome = run_repair_loop(
        &rules,
        text_subject("In conclusion, we ship."),
        RepairCap::new(0).unwrap(),
        &mut repairer,
        &Perfect,
    )
    .unwrap();
    assert_eq!(outcome.stop, StopReason::RepairCapReached);
    assert_eq!(outcome.record.disposition, Disposition::Block);
    assert_eq!(outcome.record.advisory[0].scores.len(), 1);
}

#[test]
fn clean_text_passes_whether_the_judge_scores_or_fails() {
    let rules = RuleSet::new(
        "test.text",
        1,
        vec![Rule {
            id: "det.text.banned_phrase".to_string(),
            required: true,
            kind: RuleKind::BannedPhrase {
                phrases: phrases(&["in conclusion"]),
                min_words: 1,
            },
        }],
    )
    .unwrap();
    let mut repairer = no_repairs;
    let judged = run_repair_loop(
        &rules,
        text_subject("We shipped on Tuesday."),
        RepairCap::new(0).unwrap(),
        &mut repairer,
        &Unavailable,
    )
    .unwrap();
    assert_eq!(judged.stop, StopReason::Passed);
    assert_eq!(judged.record.disposition, Disposition::Pass);
    assert!(judged.record.advisory[0].error.is_some());

    let mut repairer = no_repairs;
    let no_judge = run_repair_loop(
        &rules,
        text_subject("We shipped on Tuesday."),
        RepairCap::new(0).unwrap(),
        &mut repairer,
        &NoJudge,
    )
    .unwrap();
    assert_eq!(no_judge.record.disposition, Disposition::Pass);
}
