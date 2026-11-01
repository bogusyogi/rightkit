//! Measured style scenarios (#93): a finished reference edit over a synthetic
//! transcript with known gaps measures to the expected values, and every
//! measured value carries the source BLAKE3 in its provenance.

use rightkit_autoedit::cuts::{EditingStyle, FillerPolicy, GapMs, Pace, Preset, Provenance};
use rightkit_transcript::{EndSource, ModelRef, Span, Transcript, Word, LEGACY_SCHEMA as SCHEMA};

const SOURCE_BLAKE3: &str = "7a1c0f3b9e2d4a60c5b8f1e7d3a9c2b40f6e8d1a2c3b4e5f6a7b8c9d0e1f2a3b";

fn word(text: &str, start_ms: u64, end_ms: u64) -> Word {
    Word {
        text: text.into(),
        start_ms,
        end_ms,
        confidence: None,
        end_source: EndSource::Model,
    }
}

/// Synthetic talk. Kept words and gaps between them (ms):
///   "Hello"[100-400] gap 100 "there."[500-900] gap 200 "um"[1100-1300]
///   gap 300 "this"[1600-1800] gap 400 "is"[2200-2300] gap 500 "great."[2800-3200]
/// Then "uh"[3500-3700] is outside the kept range and is cut.
fn synthetic() -> Transcript {
    Transcript {
        schema: SCHEMA.into(),
        source_blake3: SOURCE_BLAKE3.into(),
        language: "en".into(),
        duration_ms: 10_000,
        words: vec![
            word("Hello", 100, 400),
            word("there.", 500, 900),
            word("um", 1100, 1300),
            word("this", 1600, 1800),
            word("is", 2200, 2300),
            word("great.", 2800, 3200),
            word("uh", 3500, 3700),
        ],
        vad: vec![Span {
            start_ms: 0,
            end_ms: 5_000,
        }],
        model: ModelRef {
            id: "m".into(),
            revision: "r".into(),
            backend: "coreml".into(),
        },
    }
}

/// One kept range from 50 ms to 3400 ms: lead-in 50 ms, lead-out 200 ms.
const KEPT: [(i64, i64); 1] = [(50, 3400)];

fn measured_method(prov: &Provenance) -> (&str, &str) {
    match prov {
        Provenance::Measured {
            method,
            source_hash,
        } => (method.as_str(), source_hash.as_str()),
        other => panic!("expected measured provenance, got {other:?}"),
    }
}

#[test]
fn known_gaps_measure_to_expected_values() {
    let (style, m) = EditingStyle::measure(&synthetic(), &KEPT, Preset::Natural).unwrap();

    // Five kept pauses: 100, 200, 300, 400, 500 ms.
    assert_eq!(m.kept_gap_count, 5);
    assert_eq!(m.median_gap_ms, Some(300));
    assert_eq!(m.p90_gap_ms, Some(500));
    assert_eq!(style.max_pause.value, GapMs(500));

    // Only "there." ends a sentence inside the kept range; its pause is 200 ms.
    assert_eq!(m.sentence_gap_count, 1);
    assert_eq!(style.sentence_pause.value, GapMs(200));

    // Lead-in 100 - 50 = 50 ms; lead-out 3400 - 3200 = 200 ms.
    assert_eq!(m.median_lead_in_ms, Some(50));
    assert_eq!(m.median_lead_out_ms, Some(200));
    assert_eq!(style.lead_in.value, GapMs(50));
    assert_eq!(style.tail_out.value, GapMs(200));

    // Six kept words over 3350 ms: 6 * 60000 / 3350 = 107.46, rounds to 107.
    assert_eq!(m.kept_words, 6);
    assert_eq!(m.kept_duration_ms, 3350);
    assert_eq!(m.wpm, Some(107));
    assert_eq!(style.pace.as_ref().unwrap().value, Pace(107));
}

#[test]
fn filler_rate_and_policy_follow_the_reference_edit() {
    let (style, m) = EditingStyle::measure(&synthetic(), &KEPT, Preset::Natural).unwrap();

    // "um" is kept, "uh" is cut: one kept filler in six kept words.
    assert_eq!(m.filler_hits_total, 2);
    assert_eq!(m.filler_hits_kept, 1);
    let rate = m.filler_rate_per_100_kept_words;
    assert!((rate - 100.0 / 6.0).abs() < 1e-9, "rate was {rate}");
    assert_eq!(style.fillers.value, FillerPolicy::Keep);
}

#[test]
fn provenance_carries_the_source_blake3_and_a_method_per_value() {
    let transcript = synthetic();
    let (style, _) = EditingStyle::measure(&transcript, &KEPT, Preset::Natural).unwrap();

    let (method, hash) = measured_method(&style.max_pause.provenance);
    assert_eq!(method, "reference-kept-pause-p90-v1");
    assert_eq!(hash, transcript.source_blake3);
    assert_eq!(hash, SOURCE_BLAKE3);

    for prov in [
        &style.sentence_pause.provenance,
        &style.lead_in.provenance,
        &style.tail_out.provenance,
        &style.fillers.provenance,
        &style.pace.as_ref().unwrap().provenance,
    ] {
        let (_, hash) = measured_method(prov);
        assert_eq!(hash, SOURCE_BLAKE3);
    }
}

#[test]
fn unobservable_values_keep_their_authored_preset_provenance() {
    let (style, _) = EditingStyle::measure(&synthetic(), &KEPT, Preset::Natural).unwrap();
    for prov in [
        &style.release_pad.provenance,
        &style.attack_pad.provenance,
        &style.min_cut.provenance,
        &style.filler_isolation.provenance,
    ] {
        assert!(
            matches!(prov, Provenance::Authored { .. }),
            "expected authored, got {prov:?}"
        );
    }
}

#[test]
fn measured_style_round_trips_with_provenance() {
    let (style, _) = EditingStyle::measure(&synthetic(), &KEPT, Preset::Natural).unwrap();
    let json = serde_json::to_string(&style).unwrap();
    assert!(json.contains("\"kind\":\"measured\""));
    assert!(json.contains(SOURCE_BLAKE3));
    let back: EditingStyle = serde_json::from_str(&json).unwrap();
    assert_eq!(back, style);
}

#[test]
fn a_single_kept_pause_is_both_median_and_p90() {
    // Only the first two words are kept: one pause (100 ms). The sentence-final
    // word "there." ends the kept range, so it contributes no sentence pause.
    let (style, m) = EditingStyle::measure(&synthetic(), &[(50, 950)], Preset::Natural).unwrap();
    assert_eq!(m.kept_gap_count, 1);
    assert_eq!(m.median_gap_ms, Some(100));
    assert_eq!(style.max_pause.value, GapMs(100));
}

#[test]
fn empty_reference_edit_is_an_error() {
    let err = EditingStyle::measure(&synthetic(), &[], Preset::Natural).unwrap_err();
    assert_eq!(err.to_string(), "the reference edit keeps no ranges");

    let err = EditingStyle::measure(&synthetic(), &[(9_000, 9_500)], Preset::Natural).unwrap_err();
    assert_eq!(
        err.to_string(),
        "the reference edit keeps no transcript words"
    );
}
