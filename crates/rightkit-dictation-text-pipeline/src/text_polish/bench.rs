use super::*;
use std::time::Instant;

#[test]
fn spelling_correction_removed_leaves_words_verbatim() {
    // Local spelling correction was cut (dispatch #6): ASR emits real words,
    // so a spellchecker can't fix its mis-recognitions and only risks
    // corrupting valid rare words. `polish_clean` is now identity.
    assert_eq!(polish_clean("teh report"), "teh report");
    assert_eq!(polish_clean("i seen the report"), "i seen the report");
    // Brand repair is a SEPARATE layer the host installs.
    use rightkit_text_polish::text_pipeline::{BrandHomophones, HomophoneGuard};
    configure_brand_homophones(BrandHomophones {
        brand: "HeardRight",
        phrases: &[(
            "heard right",
            HomophoneGuard::LiteralAfter(&["i", "you", "we"]),
        )],
        context_terms: &[],
    });
    assert_eq!(
        polish("heard right uses squarespace"),
        "HeardRight uses Squarespace"
    );
}

#[test]
fn continuation_casing_follows_the_field_tail() {
    // Field ends mid-sentence -> the dictation continues it: no forced
    // capital on the first word. Sentence-ending tails keep the capital.
    assert!(continues_mid_sentence(Some("I typed two words")));
    assert!(continues_mid_sentence(Some("trailing space  ")));
    assert!(!continues_mid_sentence(Some("A finished sentence.")));
    assert!(!continues_mid_sentence(Some("He said \"stop.\"")));
    assert!(!continues_mid_sentence(Some("a list:\n")));
    assert!(!continues_mid_sentence(Some("   ")));
    assert!(!continues_mid_sentence(None));
    assert!(!continues_mid_sentence(Some("Do anything")));
    assert!(!continues_mid_sentence(Some("  Ask anything…  ")));
    assert!(continues_mid_sentence(Some("Do anything useful")));

    let continued = polish_local_only_with("and then it worked", false);
    assert!(
        continued.starts_with("and"),
        "continuation must stay lowercase: {continued}"
    );
    // ASR emits sentence-cased text — a continuation must ACTIVELY
    // downcase it, not merely refrain from capitalizing (field bug
    // 2026-07-16: "Continuing a sentence" delivered with the capital).
    let downcased = polish_local_only_with("Continuing a sentence works now", false);
    assert!(
        downcased.starts_with("continuing"),
        "ASR-capitalized continuation must be downcased: {downcased}"
    );
    // The pronoun-I family is exempt from the downcase.
    let pronoun = polish_local_only_with("I tried it again", false);
    assert!(
        pronoun.starts_with("I "),
        "pronoun I must stay capitalized: {pronoun}"
    );
    let contraction = polish_local_only_with("I'm still testing", false);
    assert!(
        contraction.starts_with("I'm"),
        "I-contraction must stay capitalized: {contraction}"
    );
    let fresh = polish_local_only_with("and then it worked", true);
    assert!(
        fresh.starts_with("And"),
        "fresh sentence must capitalize: {fresh}"
    );
}

#[test]
fn local_fallback_adds_visual_sentence_chunks() {
    let output = polish_local_only_with(
        "Alpha bravo charlie delta echo foxtrot golf hotel india juliet. Kilo lima mike november oscar papa quebec romeo sierra tango? Uniform victor whiskey xray yankee zulu amber bronze copper diamond! Eagle forest garden harbor island jungle lantern meadow nickel ocean.",
        true,
    );
    assert!(
        output.lines().count() >= 2,
        "long prose should be visually chunked: {output}"
    );
    assert!(output.lines().all(|line| !line.is_empty()));
    assert!(!output.contains("\n\n"));
}

#[test]
fn domain_spacing_survives_aggressive_cleanup() {
    // "dot com" -> ".com" is a deterministic L0 rule, but aggressive cleanup
    // can re-space the domain dot; polish must re-tighten it.
    let out = polish("email me at adrian at example dot com tomorrow");
    assert!(
        out.contains("example.com"),
        "domain split by cleanup: {out}"
    );
    assert!(!out.contains(". com"), "space before TLD: {out}");
}

#[test]
fn l1_app_polish_gate_skips_commands_and_short_audio() {
    assert!(!should_try_l1_app_polish(
        "select all",
        DictationPolishContext {
            audio_secs: Some(8.0),
            ..Default::default()
        }
    ));
    assert!(!should_try_l1_app_polish(
        "open chrome",
        DictationPolishContext {
            audio_secs: Some(8.0),
            ..Default::default()
        }
    ));
    assert!(!should_try_l1_app_polish(
        "The inverse came to $4,250 due tomorrow morning please review it.",
        DictationPolishContext {
            audio_secs: Some(2.9),
            ..Default::default()
        }
    ));
    assert!(should_try_l1_app_polish(
        "The inverse came to $4,250 due tomorrow morning please review it.",
        DictationPolishContext {
            audio_secs: Some(3.1),
            ..Default::default()
        }
    ));
}

#[test]
fn local_aggressive_runs_after_deterministic_cleanup() {
    let deterministic = rightkit_text_polish::text_pipeline::deterministic_polish(
        "so um i mean we should you know ship this today",
    );
    // L0 normalises spacing, casing, and punctuation; it does not delete
    // discourse words (see fix(l0): stop deleting discourse words).
    assert_eq!(
        polish_aggressive(&deterministic),
        "So um I mean we should you know ship this today"
    );
}

#[test]
fn local_clean_runs_after_deterministic_cleanup() {
    let deterministic = rightkit_text_polish::text_pipeline::deterministic_polish(
        "so um i mean we should you know ship this today",
    );
    assert_eq!(
        polish_clean(&deterministic),
        "So um I mean we should you know ship this today"
    );
}

#[test]
fn normal_polish_does_not_own_summary_tail_trigger() {
    // polish_dictation reads the RIGHTKIT_L3_* env: hold the crate-wide env lock so a lane test's
    // cloud consent and scripted base URL can never reach this call (it would steal that test's reply).
    let _env = crate::test_support::test_env_guard();
    assert_eq!(
        polish_dictation(
            "the footer has too much whitespace and the logo is broken please summarize",
            DictationPolishContext {
                audio_secs: Some(8.0),
                ..Default::default()
            },
        ),
        "The footer has too much whitespace and the logo is broken please summarize"
    );
}

// Run with:
//   cargo test -p rightkit-dictation-engine clean_vs_aggressive -- --ignored --nocapture
#[test]
#[ignore]
fn clean_vs_aggressive() {
    let samples = [
        "Testing one two three",
        "Possibly the last direction in that life.",
        "And I understand Lee is the most beautiful girl in the world, so",
        "So um I think we should you know ship the build today and then \
         circle back on the latency stuff tomorrow morning if that works for everyone",
    ];

    let cold = Instant::now();
    let _ = polish_aggressive(samples[0]);
    println!(
        "aggressive cleanup cold load: {} ms",
        cold.elapsed().as_millis()
    );

    let bench = |f: &dyn Fn(&str) -> String, s: &str| -> f64 {
        let runs = 20;
        let t = Instant::now();
        for _ in 0..runs {
            let _ = f(s);
        }
        t.elapsed().as_secs_f64() * 1000.0 / runs as f64
    };

    for s in samples {
        let clean_ms = bench(&polish_clean, s);
        let agg_ms = bench(&polish_aggressive, s);
        println!(
            "[{:>3} chars] clean={:.1}ms  aggressive={:.1}ms",
            s.len(),
            clean_ms,
            agg_ms
        );
    }

    let messy = "i seen the the report and its got alot of issues we was gonna fix";
    println!("input     : {messy}");
    println!("aggressive: {}", polish_aggressive(messy));
}
