//! Self-talk scenarios (#96): a restart cue removes the abandoned attempt before it
//! and the cue itself, keeps the restart, stops at boundaries, and escalates when the
//! cue is ambiguous or has no attempt or no restart.

use rightkit_autoedit::cuts::{cut_mask, escalations, self_talk, RetakeRule, RuleConfig, Verdict};
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
fn strong_cue_removes_attempt_and_cue_keeps_restart() {
    let words = line(&[
        ("we", 0, None),
        ("ship", 0, None),
        ("on", 0, None),
        ("monday", 0, None),
        ("scratch", 0, None),
        ("that", 0, None),
        ("we", 0, None),
        ("ship", 0, None),
        ("on", 0, None),
        ("friday.", 0, None),
    ]);
    let hits = self_talk(&words, &RuleConfig::default());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rule, RetakeRule::SelfTalk);
    assert_eq!(hits[0].verdict, Verdict::Cut);
    let mask = cut_mask(words.len(), &hits);
    assert_eq!(
        texts(&words, &mask, true),
        vec!["we", "ship", "on", "monday", "scratch", "that"]
    );
    assert_eq!(
        texts(&words, &mask, false),
        vec!["we", "ship", "on", "friday."]
    );
}

#[test]
fn sentence_end_limits_the_attempt_to_the_last_sentence() {
    let words = line(&[
        ("thanks", 0, None),
        ("for", 0, None),
        ("watching.", 0, None),
        ("today", 0, None),
        ("we", 0, None),
        ("talk", 0, None),
        ("pricing.", 0, None),
        ("wait", 0, None),
        ("no,", 0, None),
        ("today", 0, None),
        ("we", 0, None),
        ("talk", 0, None),
        ("budgets.", 0, None),
    ]);
    let hits = self_talk(&words, &RuleConfig::default());
    let mask = cut_mask(words.len(), &hits);
    assert_eq!(
        texts(&words, &mask, true),
        vec!["today", "we", "talk", "pricing.", "wait", "no,"]
    );
    assert_eq!(
        texts(&words, &mask, false),
        vec![
            "thanks",
            "for",
            "watching.",
            "today",
            "we",
            "talk",
            "budgets."
        ]
    );
}

#[test]
fn pause_limits_the_attempt_to_words_after_it() {
    // A 400 ms pause before "we" ends the earlier phrase, so "okay so" is outside the attempt.
    let words = line(&[
        ("okay", 0, None),
        ("so", 0, None),
        ("we", 400, None),
        ("ship", 0, None),
        ("it", 0, None),
        ("wait", 0, None),
        ("no", 0, None),
        ("we", 0, None),
        ("ship", 0, None),
        ("friday.", 0, None),
    ]);
    let hits = self_talk(&words, &RuleConfig::default());
    let mask = cut_mask(words.len(), &hits);
    assert_eq!(
        texts(&words, &mask, true),
        vec!["we", "ship", "it", "wait", "no"]
    );
    assert_eq!(
        texts(&words, &mask, false),
        vec!["okay", "so", "we", "ship", "friday."]
    );
}

#[test]
fn ambiguous_cue_is_escalated_and_kept() {
    let words = line(&[
        ("i", 0, None),
        ("mean", 0, None),
        ("the", 0, None),
        ("blue", 0, None),
        ("one.", 0, None),
    ]);
    let hits = self_talk(&words, &RuleConfig::default());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].verdict, Verdict::Escalate);
    assert_eq!(escalations(&hits).len(), 1);
    let mask = cut_mask(words.len(), &hits);
    assert!(texts(&words, &mask, true).is_empty());
}

#[test]
fn strong_cue_without_restart_is_escalated_and_kept() {
    let words = line(&[
        ("we", 0, None),
        ("ship", 0, None),
        ("friday", 0, None),
        ("scratch", 0, None),
        ("that", 0, None),
    ]);
    let hits = self_talk(&words, &RuleConfig::default());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].verdict, Verdict::Escalate);
    let mask = cut_mask(words.len(), &hits);
    assert!(texts(&words, &mask, true).is_empty());
}
