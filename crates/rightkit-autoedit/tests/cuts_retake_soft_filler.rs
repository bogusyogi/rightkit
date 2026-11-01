//! Soft-filler keep-gate scenarios (#96): a soft filler is cut only when isolated by
//! pauses on both sides and transcribed at or above the confidence floor. Isolated
//! low-confidence fillers are kept and escalated. Missing confidence is kept.

use rightkit_autoedit::cuts::{cut_mask, escalations, soft_filler_gate, RuleConfig, Verdict};
use rightkit_transcript::{EndSource, Word};

/// Builds words back to back. Each part is (text, pause before it in ms, confidence).
fn line(parts: &[(&str, u64, Option<f32>)]) -> Vec<Word> {
    let mut out = Vec::new();
    let mut t: u64 = 0;
    for (text, gap, conf) in parts {
        let start = t + gap;
        let end = start + 300;
        out.push(Word {
            text: (*text).into(),
            start_ms: start,
            end_ms: end,
            confidence: *conf,
            end_source: EndSource::Model,
        });
        t = end;
    }
    out
}

fn texts<'a>(words: &'a [Word], mask: &[bool], want: bool) -> Vec<&'a str> {
    words
        .iter()
        .zip(mask)
        .filter(|(_, m)| **m == want)
        .map(|(w, _)| w.text.as_str())
        .collect()
}

#[test]
fn isolated_confident_filler_is_cut() {
    let words = line(&[
        ("the", 0, Some(0.95)),
        ("plan", 0, Some(0.95)),
        ("is", 0, Some(0.95)),
        ("like", 400, Some(0.92)),
        ("fast", 400, Some(0.95)),
    ]);
    let hits = soft_filler_gate(&words, &RuleConfig::default());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].verdict, Verdict::Cut);
    let mask = cut_mask(words.len(), &hits);
    assert_eq!(texts(&words, &mask, true), vec!["like"]);
    assert_eq!(
        texts(&words, &mask, false),
        vec!["the", "plan", "is", "fast"]
    );
}

#[test]
fn filler_touching_speech_on_a_side_is_kept() {
    let words = line(&[
        ("it", 0, Some(0.95)),
        ("is", 0, Some(0.95)),
        ("like", 0, Some(0.95)),
        ("fast", 0, Some(0.95)),
    ]);
    let hits = soft_filler_gate(&words, &RuleConfig::default());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].verdict, Verdict::Keep);
    let mask = cut_mask(words.len(), &hits);
    assert!(texts(&words, &mask, true).is_empty());
}

#[test]
fn isolated_low_confidence_filler_is_kept_and_escalated() {
    let words = line(&[
        ("a", 0, Some(0.95)),
        ("like", 400, Some(0.5)),
        ("b", 400, Some(0.95)),
    ]);
    let hits = soft_filler_gate(&words, &RuleConfig::default());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].verdict, Verdict::Escalate);
    assert_eq!(escalations(&hits).len(), 1);
    let mask = cut_mask(words.len(), &hits);
    assert!(texts(&words, &mask, true).is_empty());
}

#[test]
fn filler_without_confidence_is_kept() {
    let words = line(&[("a", 0, None), ("like", 400, None), ("b", 400, None)]);
    let hits = soft_filler_gate(&words, &RuleConfig::default());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].verdict, Verdict::Keep);
    let mask = cut_mask(words.len(), &hits);
    assert!(texts(&words, &mask, true).is_empty());
}

#[test]
fn multi_word_filler_is_cut_as_one_unit() {
    let words = line(&[
        ("it", 0, Some(0.9)),
        ("is", 0, Some(0.9)),
        ("you", 400, Some(0.9)),
        ("know", 0, Some(0.9)),
        ("fast", 400, Some(0.9)),
    ]);
    let hits = soft_filler_gate(&words, &RuleConfig::default());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].verdict, Verdict::Cut);
    let mask = cut_mask(words.len(), &hits);
    assert_eq!(texts(&words, &mask, true), vec!["you", "know"]);
}
