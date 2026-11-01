//! End-to-end retake scenarios (#96) through the real editorial planner: `analyze`
//! builds the plan, `resolve` realises the reviewed edit. A broken later take stays a
//! suggestion; a self-talk restart is cut through the same planner.

use rightkit_autoedit::cuts::RealiseOptions;
use rightkit_autoedit::editorial::{
    analyze, EditorialConfig, EditorialPlan, ItemKind, Review, Tier,
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

#[test]
fn broken_later_take_stays_a_suggestion_and_is_cut_only_when_accepted() {
    let full = "Welcome to the channel and today we are going to learn editing.";
    let cut = "Welcome to the channel and today we are going to";
    let t = Builder::new().say(full).pause(800).say(cut).build();
    let p = plan_of(&t);

    // The complete take is kept; the broken later take is emitted as a suggestion.
    let g = &p.take_groups[0];
    assert_eq!(g.chosen, 0, "{g:#?}");
    let broken = p
        .item(&g.takes[1].item_id)
        .expect("the broken take has a retake item");
    assert_eq!(broken.kind, ItemKind::Retake);
    assert_eq!(broken.tier, Tier::Suggest, "{broken:#?}");
    assert!(broken.reason.contains("suggested"), "{}", broken.reason);

    // Default review rejects suggestions: the broken take stays in the output.
    let default_text = kept_text(&p, &t, &Review::new());
    assert_eq!(default_text, format!("{full} {cut}"));

    // Accepting the suggestion cuts it, and the complete take survives intact.
    let mut rv = Review::new();
    rv.accept(&broken.id);
    assert_eq!(kept_text(&p, &t, &rv), full);
}

#[test]
fn self_talk_restart_is_cut_through_the_planner_and_the_restart_kept() {
    // "oops" is a strong cue that is not in the cue list, so only the self-talk rule
    // can remove the abandoned attempt before it.
    let restart = "we should ship this today.";
    let t = Builder::new()
        .say("I think the first idea is weak oops")
        .say(restart)
        .build();
    let p = plan_of(&t);

    let marked: Vec<_> = p
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::MarkedRetake)
        .collect();
    assert_eq!(marked.len(), 1, "{:#?}", p.items);
    assert_eq!(marked[0].tier, Tier::Auto);
    assert!(
        marked[0].reason.contains("abandoned attempt"),
        "{}",
        marked[0].reason
    );
    assert_eq!(kept_text(&p, &t, &Review::new()), restart);
}

/// "we ship this", an isolated "like" (pauses on both sides), "the plan"; the given
/// confidence is set on "like" only.
fn isolated_like(confidence: f32) -> Transcript {
    let mut t = Builder::new()
        .say("we ship this")
        .pause(300)
        .say("like")
        .pause(300)
        .say("the plan")
        .build();
    let i = t.words.iter().position(|w| w.text == "like").unwrap();
    t.words[i].confidence = Some(confidence);
    t
}

#[test]
fn low_confidence_isolated_like_is_kept_and_listed_for_escalation() {
    let t = isolated_like(0.4);
    let p = plan_of(&t);

    // The gate vetoes the baseline filler: no filler item exists for "like".
    assert!(
        p.items.iter().all(|i| i.kind != ItemKind::Filler),
        "{:#?}",
        p.items
    );
    let esc: Vec<_> = p
        .retake_escalations
        .iter()
        .filter(|e| e.text == "like")
        .collect();
    assert_eq!(esc.len(), 1, "{:#?}", p.retake_escalations);
    assert_eq!(esc[0].rule, "soft_filler");
    assert!(esc[0].reason.contains("escalation"), "{}", esc[0].reason);
    assert_eq!(
        kept_text(&p, &t, &Review::new()),
        "we ship this like the plan"
    );
}

#[test]
fn confident_isolated_like_is_cut() {
    let t = isolated_like(0.95);
    let p = plan_of(&t);

    assert!(
        p.retake_escalations.is_empty(),
        "{:#?}",
        p.retake_escalations
    );
    assert_eq!(kept_text(&p, &t, &Review::new()), "we ship this the plan");
}
