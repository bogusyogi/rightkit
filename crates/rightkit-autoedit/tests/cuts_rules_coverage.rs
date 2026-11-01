//! Coverage for matrix rules 6, 23 and 26 (docs/editing-rules-matrix.md).
//!
//! Scenario tests through the public `plan_cuts`, `self_talk` and `cut_mask` API on
//! hand-built transcripts. No e2e is needed: the transcript is the evidence and the
//! planner is pure computation, so the rules are driven directly.

use rightkit_autoedit::cuts::{
    cut_mask, escalations, plan_cuts, self_talk, CutConfig, CutPlan, RemovalReason, RetakeRule,
    RuleConfig, RuleHit, Verdict,
};
use rightkit_timeline::Fps;
use rightkit_transcript::{EndSource, ModelRef, Span, Transcript, Word, LEGACY_SCHEMA as SCHEMA};

const HEX: &str = "abababababababababababababababababababababababababababababababab";

fn fps() -> Fps {
    Fps::new(30, 1).unwrap()
}

fn w(text: &str, s: u64, e: u64) -> Word {
    Word {
        text: text.into(),
        start_ms: s,
        end_ms: e,
        confidence: None,
        end_source: EndSource::Model,
    }
}

/// Runs of words separated by less than 150 ms form one VAD span.
fn tr(words: Vec<Word>, duration_ms: u64) -> Transcript {
    let mut vad: Vec<Span> = Vec::new();
    for x in &words {
        match vad.last_mut() {
            Some(l) if x.start_ms - l.end_ms < 150 => l.end_ms = x.end_ms,
            _ => vad.push(Span {
                start_ms: x.start_ms,
                end_ms: x.end_ms,
            }),
        }
    }
    Transcript {
        schema: SCHEMA.into(),
        source_blake3: HEX.into(),
        language: "en".into(),
        duration_ms,
        words,
        vad,
        model: ModelRef {
            id: "m".into(),
            revision: "r".into(),
            backend: "synthetic".into(),
        },
    }
}

/// Words back to back. Each part is (text, pause before it in ms, confidence).
fn line(parts: &[(&str, u64)]) -> Vec<Word> {
    let mut out = Vec::new();
    let mut t: u64 = 0;
    for (text, gap) in parts {
        let start = t + gap;
        let end = start + 300;
        out.push(Word {
            text: (*text).into(),
            start_ms: start,
            end_ms: end,
            confidence: None,
            end_source: EndSource::Model,
        });
        t = end;
    }
    out
}

fn kept_start(p: &CutPlan) -> i64 {
    p.kept.first().expect("something is kept").start_ms()
}

fn kept_end(p: &CutPlan) -> i64 {
    p.kept.last().expect("something is kept").end_ms()
}

fn texts<'a>(words: &'a [Word], mask: &[bool], want: bool) -> Vec<&'a str> {
    words
        .iter()
        .zip(mask)
        .filter(|(_, m)| **m == want)
        .map(|(w, _)| w.text.as_str())
        .collect()
}

// Rule 6: slivers below `min_cut_ms` (40 ms on tight) are not cut.
//
// Tight: lead_in 80 ms, tail_out 130 ms, min_cut 40 ms. A leading sliver of 100 ms
// keeps 80 ms, which leaves 20 ms to trim, below min_cut, so nothing is cut. Only the
// planner's own min_cut gate can produce this outcome from public input, so this is a
// scenario and not a unit test. The trimmed twin shows the gate is the only difference.

#[test]
fn rule_6_leading_sliver_is_not_cut() {
    let sliver = tr(vec![w("hello", 100, 400)], 1000);
    let p = plan_cuts(&sliver, None, &CutConfig::tight(), fps()).unwrap();
    assert!(
        !p.decisions
            .iter()
            .any(|d| matches!(d.kind, RemovalReason::Silence) && d.start_ms == 0),
        "a 20 ms leading sliver must not be cut"
    );
    assert_eq!(kept_start(&p), 0, "kept range starts at the file start");

    // 400 ms leading silence keeps 80 ms and trims 320 ms, well above min_cut.
    let trimmed = tr(vec![w("hello", 400, 700)], 1000);
    let p = plan_cuts(&trimmed, None, &CutConfig::tight(), fps()).unwrap();
    assert!(
        p.decisions
            .iter()
            .any(|d| matches!(d.kind, RemovalReason::Silence) && d.start_ms == 0),
        "a 320 ms leading silence must be cut"
    );
    assert!(kept_start(&p) > 200, "kept start {}", kept_start(&p));
}

#[test]
fn rule_6_trailing_sliver_is_not_cut() {
    // Word ends at 500 of 650 ms. tail_out keeps 130 ms, leaving 20 ms: below min_cut.
    let sliver = tr(vec![w("hello", 100, 500)], 650);
    let p = plan_cuts(&sliver, None, &CutConfig::tight(), fps()).unwrap();
    assert!(
        !p.decisions
            .iter()
            .any(|d| matches!(d.kind, RemovalReason::Silence) && d.start_ms >= 500),
        "a 20 ms trailing sliver must not be cut"
    );
    assert!(kept_end(&p) >= 640, "kept end {}", kept_end(&p));

    // Word ends at 500 of 800 ms. 170 ms of trailing silence is above min_cut and trimmed.
    let trimmed = tr(vec![w("hello", 100, 500)], 800);
    let p = plan_cuts(&trimmed, None, &CutConfig::tight(), fps()).unwrap();
    assert!(
        p.decisions
            .iter()
            .any(|d| matches!(d.kind, RemovalReason::Silence) && d.end_ms == 800),
        "170 ms trailing silence must be cut"
    );
    assert!(kept_end(&p) <= 700, "kept end {}", kept_end(&p));
}

// Rule 23: an abandoned attempt is capped at `max_attempt_words` (12 by default).
//
// The rule is a cap, not a refusal: attempt_start stops 12 words before the cue, so
// words further back are kept. The cap is only reachable through self_talk on a run of
// words with no sentence end and no pause, which is why this is a scenario.

#[test]
fn rule_23_attempt_cap_keeps_words_beyond_twelve() {
    let mut parts: Vec<(String, u64)> = (0..20).map(|i| (format!("w{i}"), 0)).collect();
    parts.push(("scratch".into(), 0));
    parts.push(("that".into(), 0));
    parts.push(("we".into(), 0));
    parts.push(("ship".into(), 0));
    parts.push(("friday.".into(), 0));
    let refs: Vec<(&str, u64)> = parts.iter().map(|(t, g)| (t.as_str(), *g)).collect();
    let words = line(&refs);
    assert_eq!(words.len(), 25);

    let hits = self_talk(&words, &RuleConfig::default());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].verdict, Verdict::Cut);
    // Cue sits at index 20; 12 words back from it is index 8.
    assert_eq!(hits[0].first, 8, "attempt capped at 12 words");
    let mask = cut_mask(words.len(), &hits);
    let kept = texts(&words, &mask, false);
    assert_eq!(
        &kept[..8],
        &["w0", "w1", "w2", "w3", "w4", "w5", "w6", "w7"],
        "words beyond the cap survive"
    );
    assert_eq!(&kept[8..], &["we", "ship", "friday."]);

    // With the cap raised to cover the whole run, the attempt is removed from the start.
    let wide = RuleConfig {
        max_attempt_words: 30,
        ..RuleConfig::default()
    };
    let hits = self_talk(&words, &wide);
    assert_eq!(hits[0].first, 0);
}

// Rule 26: only words marked Cut are removed; every other word is kept.
//
// Scenario: a restart cue removes its attempt, and an ambiguous "sorry" is escalated.
// The escalated word must survive the mask. The clamp check uses hand-built hits to
// show that a Cut range past the transcript end and an Escalate hit never remove words.

#[test]
fn rule_26_only_cut_words_are_removed() {
    let words = line(&[
        ("we", 0),
        ("ship", 0),
        ("monday", 0),
        ("scratch", 0),
        ("that", 0),
        ("we", 0),
        ("ship", 0),
        ("friday.", 0),
        ("sorry", 0),
        ("the", 0),
        ("budget", 0),
        ("is", 0),
        ("set.", 0),
    ]);
    let hits = self_talk(&words, &RuleConfig::default());
    let mask = cut_mask(words.len(), &hits);
    assert_eq!(
        texts(&words, &mask, true),
        vec!["we", "ship", "monday", "scratch", "that"]
    );
    assert_eq!(escalations(&hits).len(), 1);
    assert_eq!(escalations(&hits)[0].verdict, Verdict::Escalate);
    assert!(texts(&words, &mask, false).contains(&"sorry"));

    let hand = vec![
        RuleHit {
            rule: RetakeRule::SelfTalk,
            first: 1,
            end: 10,
            verdict: Verdict::Cut,
            reason: "past the end".into(),
        },
        RuleHit {
            rule: RetakeRule::SelfTalk,
            first: 0,
            end: 1,
            verdict: Verdict::Escalate,
            reason: "kept".into(),
        },
    ];
    assert_eq!(cut_mask(3, &hand), vec![false, true, true]);
}
