//! Scenario tests for editing-rules matrix rows 31, 33, 35 and 39, driven through the
//! editorial planner's public API: `analyze` builds the plan and `resolve` realises the
//! reviewed edit. Transcript construction follows `retake_rules_e2e.rs`.

use rightkit_autoedit::cuts::RealiseOptions;
use rightkit_autoedit::editorial::{
    analyze, EditorialConfig, EditorialError, EditorialPlan, ItemKind, Review, Tier,
};
use rightkit_timeline::Fps;
use rightkit_transcript::{EndSource, ModelRef, Span, Transcript, Word, LEGACY_SCHEMA as TSCHEMA};

const HEX: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

fn fps() -> Fps {
    Fps::new(30, 1).unwrap()
}

/// Synthetic transcript: 300 ms words, 60 ms between words unless a pause is added.
struct Builder {
    t: u64,
    words: Vec<Word>,
}

impl Builder {
    fn new() -> Self {
        Self {
            t: 500,
            words: Vec::new(),
        }
    }

    fn say(mut self, text: &str) -> Self {
        for w in text.split_whitespace() {
            self.words.push(Word {
                text: w.into(),
                start_ms: self.t,
                end_ms: self.t + 300,
                confidence: None,
                end_source: EndSource::Model,
            });
            self.t += 360;
        }
        self
    }

    fn pause(mut self, ms: u64) -> Self {
        self.t += ms;
        self
    }

    fn build(self) -> Transcript {
        let duration_ms = self.t + 500;
        let mut vad: Vec<Span> = Vec::new();
        for x in &self.words {
            match vad.last_mut() {
                Some(l) if x.start_ms - l.end_ms < 150 => l.end_ms = x.end_ms,
                _ => vad.push(Span {
                    start_ms: x.start_ms,
                    end_ms: x.end_ms,
                }),
            }
        }
        Transcript {
            schema: TSCHEMA.into(),
            source_blake3: HEX.into(),
            language: "en".into(),
            duration_ms,
            words: self.words,
            vad,
            model: ModelRef {
                id: "m".into(),
                revision: "r".into(),
                backend: "synthetic".into(),
            },
        }
    }
}

fn plan_of(t: &Transcript) -> EditorialPlan {
    analyze(t, None, &EditorialConfig::natural(), fps()).unwrap()
}

/// Kept text after the review's verdicts are realised.
fn kept_text(p: &EditorialPlan, t: &Transcript, rv: &Review) -> String {
    let r = p.resolve(t, None, rv, &RealiseOptions::default()).unwrap();
    r.cut_plan
        .kept_words
        .iter()
        .map(|&i| t.words[i].text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn marked_retakes(p: &EditorialPlan) -> Vec<&rightkit_autoedit::editorial::EditorialItem> {
    p.items
        .iter()
        .filter(|i| i.kind == ItemKind::MarkedRetake)
        .collect()
}

// Row 31: retake tier by similarity. Empty attempt is Auto 0.8; similar attempt is
// Auto 0.9; dissimilar attempt is Suggest 0.55.

#[test]
fn rule_31_empty_attempt_is_auto_cut_at_0_8() {
    // The cue opens the transcript, so there is no attempt before it.
    let t = Builder::new().say("take two we ship this today.").build();
    let p = plan_of(&t);

    let marked = marked_retakes(&p);
    assert_eq!(marked.len(), 1, "{:#?}", p.items);
    assert_eq!(marked[0].tier, Tier::Auto, "{:#?}", marked[0]);
    assert!(
        (marked[0].confidence - 0.8).abs() < 1e-6,
        "{}",
        marked[0].confidence
    );
    // Auto is accepted by default: the cue is removed and the restart kept.
    assert_eq!(kept_text(&p, &t, &Review::new()), "we ship this today.");
}

#[test]
fn rule_31_similar_attempt_is_auto_cut_at_0_9() {
    // The restart repeats the abandoned attempt word for word (similarity 1.0).
    let t = Builder::new()
        .say("we ship this plan take two we ship this plan")
        .build();
    let p = plan_of(&t);

    let marked = marked_retakes(&p);
    assert_eq!(marked.len(), 1, "{:#?}", p.items);
    assert_eq!(marked[0].tier, Tier::Auto, "{:#?}", marked[0]);
    assert!(
        (marked[0].confidence - 0.9).abs() < 1e-6,
        "{}",
        marked[0].confidence
    );
    assert_eq!(kept_text(&p, &t, &Review::new()), "we ship this plan");
}

#[test]
fn rule_31_dissimilar_attempt_is_only_suggested_at_0_55() {
    // No shared words and a different first word: similarity 0 and not similar.
    let full = "red green blue take two cat dog fish";
    let t = Builder::new().say(full).build();
    let p = plan_of(&t);

    let marked = marked_retakes(&p);
    assert_eq!(marked.len(), 1, "{:#?}", p.items);
    assert_eq!(marked[0].tier, Tier::Suggest, "{:#?}", marked[0]);
    assert!(
        (marked[0].confidence - 0.55).abs() < 1e-6,
        "{}",
        marked[0].confidence
    );
    // A suggestion is not accepted by default, so nothing is removed.
    assert_eq!(kept_text(&p, &t, &Review::new()), full);
    // Accepting the suggestion removes the attempt and cue.
    let mut rv = Review::new();
    rv.accept(&marked[0].id);
    assert_eq!(kept_text(&p, &t, &rv), "cat dog fish");
}

// Row 33: a pause-only break inside a correction is a suggestion, never automatic.

#[test]
fn rule_33_pause_only_break_in_a_correction_is_suggested() {
    // No cue and no dash: the only signal is a 400 ms pause after "the". The restart
    // "we ship this" is similar to "we ship the" (dice 0.67) but not identical, so the
    // correction is not a plain repetition.
    let t = Builder::new()
        .say("we ship the")
        .pause(400)
        .say("we ship this plan")
        .build();
    let p = plan_of(&t);

    let corr: Vec<_> = p
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::SelfCorrection)
        .collect();
    assert_eq!(corr.len(), 1, "{:#?}", p.items);
    assert_eq!(corr[0].tier, Tier::Suggest, "{:#?}", corr[0]);
    assert!(
        corr[0]
            .evidence
            .notes
            .iter()
            .any(|n| n == "pause-only break"),
        "{:#?}",
        corr[0].evidence
    );
    // Default review keeps the abandoned clause; accepting cuts it.
    assert_eq!(
        kept_text(&p, &t, &Review::new()),
        "we ship the we ship this plan"
    );
    let mut rv = Review::new();
    rv.accept(&corr[0].id);
    assert_eq!(kept_text(&p, &t, &rv), "we ship this plan");
}

// Row 35: a repeated word is removed automatically and the last copy is kept.

#[test]
fn rule_35_repeated_word_is_removed_and_last_copy_kept() {
    let t = Builder::new().say("we ship the the plan").build();
    let p = plan_of(&t);

    let rep: Vec<_> = p
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Repetition)
        .collect();
    assert_eq!(rep.len(), 1, "{:#?}", p.items);
    assert_eq!(rep[0].tier, Tier::Auto, "{:#?}", rep[0]);
    assert!(rep[0].reason.contains("repeated 2x"), "{}", rep[0].reason);
    assert_eq!(kept_text(&p, &t, &Review::new()), "we ship the plan");
}

// Row 39: a verdict set that does not validate against the plan is refused.

#[test]
fn rule_39_review_naming_an_unknown_item_is_refused() {
    let t = Builder::new().say("we ship the the plan").build();
    let p = plan_of(&t);

    // A valid review resolves, so the refusal below is caused by the unknown id alone.
    assert_eq!(kept_text(&p, &t, &Review::new()), "we ship the plan");

    let mut rv = Review::new();
    rv.accept("no-such-item");
    match p.resolve(&t, None, &rv, &RealiseOptions::default()) {
        Err(e) => assert_eq!(e, EditorialError::UnknownItem("no-such-item".into())),
        Ok(_) => panic!("a review naming an unknown item must not resolve"),
    }
}
