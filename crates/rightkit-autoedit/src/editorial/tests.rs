use crate::cuts::{Preset, RealiseOptions};
use rightkit_timeline::Fps;
use rightkit_transcript::{EndSource, ModelRef, Span, Transcript, Word, LEGACY_SCHEMA as TSCHEMA};

use crate::editorial::*;

const HEX: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

fn fps() -> Fps {
    Fps::new(30, 1).unwrap()
}

/// Builds synthetic transcripts: 300 ms words, 60 ms between them.
struct B {
    t: u64,
    words: Vec<Word>,
}

impl B {
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

fn plan_of(t: &Transcript, env: Option<&[f32]>, cfg: &EditorialConfig) -> EditorialPlan {
    analyze(t, env, cfg, fps()).unwrap()
}

fn kinds(p: &EditorialPlan) -> Vec<ItemKind> {
    p.items.iter().map(|i| i.kind).collect()
}

fn of_kind(p: &EditorialPlan, k: ItemKind) -> Vec<&EditorialItem> {
    p.items.iter().filter(|i| i.kind == k).collect()
}

fn texts(t: &Transcript, r: &ResolvedEdit) -> String {
    r.cut_plan
        .kept_words
        .iter()
        .map(|&i| t.words[i].text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn kept_ms(r: &ResolvedEdit) -> i64 {
    r.cut_plan
        .kept
        .iter()
        .map(|k| k.end_ms() - k.start_ms())
        .sum()
}

/// Kept words sit whole inside kept ranges; removed words never leak in.
fn assert_words_intact(r: &ResolvedEdit, t: &Transcript) {
    let kept: Vec<(i64, i64)> = r
        .cut_plan
        .kept
        .iter()
        .map(|k| (k.start_ms(), k.end_ms()))
        .collect();
    for (i, w) in t.words.iter().enumerate() {
        let (s, e) = (w.start_ms as i64, w.end_ms as i64);
        let inside = kept.iter().any(|&(a, b)| s >= a && e <= b);
        let overlaps = kept.iter().any(|&(a, b)| s < b && e > a);
        if r.cut_plan.kept_words.contains(&i) {
            assert!(inside, "kept word {:?} not whole in {kept:?}", w.text);
        } else {
            assert!(!overlaps, "removed word {:?} leaks into {kept:?}", w.text);
        }
    }
}

fn resolve(p: &EditorialPlan, t: &Transcript, env: Option<&[f32]>, rv: &Review) -> ResolvedEdit {
    p.resolve(t, env, rv, &RealiseOptions::default()).unwrap()
}

/// Per-10 ms envelope: `floor` everywhere, `level(i)` over word i.
fn envelope(t: &Transcript, floor: f32, level: impl Fn(usize) -> f32) -> Vec<f32> {
    let mut e = vec![floor; (t.duration_ms / 10) as usize + 1];
    for (i, w) in t.words.iter().enumerate() {
        for f in (w.start_ms / 10)..(w.end_ms / 10) {
            e[f as usize] = level(i);
        }
    }
    e
}

// ---------------------------------------------------------------- pauses

#[test]
fn silence_items_shorten_pauses_and_reject_restores_them() {
    let t = B::new()
        .say("Hello there")
        .pause(2000)
        .say("friends.")
        .build();
    let cfg = EditorialConfig::tight();
    let p = plan_of(&t, None, &cfg);
    let sil = of_kind(&p, ItemKind::Silence);
    assert!(!sil.is_empty());
    assert!(sil
        .iter()
        .all(|i| i.tier == Tier::Auto && i.category() == Category::Pause));
    let all = resolve(&p, &t, None, &Review::new());
    assert_words_intact(&all, &t);
    let none = resolve(&p, &t, None, &Review::reject_all(&p));
    assert_words_intact(&none, &t);
    assert!(kept_ms(&none) > kept_ms(&all) + 1500);
    // Reject only the long inter-word pause: it comes back, the rest stays cut.
    let mid = sil
        .iter()
        .find(|i| {
            i.evidence
                .notes
                .iter()
                .any(|n| n == "between words" && i.end_ms - i.start_ms > 1000)
        })
        .unwrap();
    let mut rv = Review::new();
    rv.reject(&mid.id);
    let part = resolve(&p, &t, None, &rv);
    assert_words_intact(&part, &t);
    assert!(kept_ms(&part) > kept_ms(&all) + 1000);
    assert!(kept_ms(&part) < kept_ms(&none));
    let out = part.outcomes.iter().find(|o| o.item_id == mid.id).unwrap();
    assert!(!out.accepted && out.action_indices.is_empty());
}

#[test]
fn long_pause_item_when_silence_removal_is_off() {
    let t = B::new().say("One two").pause(3500).say("three.").build();
    let mut cfg = EditorialConfig::natural();
    cfg.cuts.remove_silence = false;
    let p = plan_of(&t, None, &cfg);
    let lp = of_kind(&p, ItemKind::LongPause);
    assert_eq!(lp.len(), 1);
    let r = resolve(&p, &t, None, &Review::new());
    assert_words_intact(&r, &t);
    assert!(kept_ms(&r) < t.duration_ms as i64 - 2500);
    assert!(r
        .outcomes
        .iter()
        .any(|o| o.item_id == lp[0].id && o.removed_ms > 2500));
}

// ------------------------------------------------------------- dead air

#[test]
fn breath_in_a_gap_is_removed_and_levels_are_evidence() {
    let t = B::new()
        .say("This is the first part of it")
        .pause(1200)
        .say("and this is the second.")
        .build();
    let gap_start = t.words[6].end_ms;
    let mut env = envelope(&t, 0.01, |_| 0.5);
    for f in ((gap_start + 400) / 10)..((gap_start + 850) / 10) {
        env[f as usize] = 0.15;
    }
    let mut cfg = EditorialConfig::natural();
    cfg.cuts.remove_silence = false;
    let p = plan_of(&t, Some(&env), &cfg);
    let b = of_kind(&p, ItemKind::Breath);
    assert_eq!(b.len(), 1, "{:?}", kinds(&p));
    assert_eq!(b[0].tier, Tier::Auto);
    assert!(b[0].evidence.signals["breath_ms"] >= 400.0);
    assert!(b[0].start_ms as u64 >= gap_start && b[0].end_ms as u64 <= t.words[7].start_ms);
    let r = resolve(&p, &t, Some(&env), &Review::new());
    assert_words_intact(&r, &t);
    assert!(
        r.cut_plan
            .kept
            .iter()
            .all(|k| k.end_ms() <= gap_start as i64 + 420
                || k.start_ms() >= gap_start as i64 + 780),
        "{:?}",
        r.cut_plan.kept
    );
    let o = r.outcomes.iter().find(|o| o.item_id == b[0].id).unwrap();
    assert!(o.accepted && o.removed_ms >= 400 && !o.action_indices.is_empty());
    assert!(o.action_indices.iter().all(|&i| i < r.group.actions.len()));
    // A short breath is only suggested.
    let mut env2 = envelope(&t, 0.01, |_| 0.5);
    for f in ((gap_start + 400) / 10)..((gap_start + 660) / 10) {
        env2[f as usize] = 0.15;
    }
    let p2 = plan_of(&t, Some(&env2), &cfg);
    let b2 = of_kind(&p2, ItemKind::Breath);
    assert_eq!(b2.len(), 1);
    assert_eq!(b2[0].tier, Tier::Suggest);
    let r2 = resolve(&p2, &t, Some(&env2), &Review::new());
    assert!(kept_ms(&r2) > kept_ms(&r));
    // No envelope, no breath items.
    assert!(of_kind(&plan_of(&t, None, &cfg), ItemKind::Breath).is_empty());
}

#[test]
fn quiet_runs_are_low_energy_or_off_mic() {
    let t = B::new()
        .say("We start with a clear strong sentence here")
        .say("honestly I think maybe so")
        .say("and then we finish the main point loudly.")
        .build();
    let quiet = 7..12;
    let off = envelope(&t, 0.01, |i| if quiet.contains(&i) { 0.1 } else { 0.5 });
    let mid = envelope(&t, 0.01, |i| if quiet.contains(&i) { 0.2 } else { 0.5 });
    let mut cfg = EditorialConfig::tight();
    cfg.cuts.remove_silence = false;
    let p = plan_of(&t, Some(&off), &cfg);
    let om = of_kind(&p, ItemKind::OffMic);
    assert_eq!(om.len(), 1, "{:?}", kinds(&p));
    assert_eq!(om[0].evidence.word_range, Some([7, 12]));
    assert_eq!(om[0].tier, Tier::Suggest);
    let p2 = plan_of(&t, Some(&mid), &cfg);
    assert_eq!(of_kind(&p2, ItemKind::LowEnergy).len(), 1);
    assert!(of_kind(&p2, ItemKind::OffMic).is_empty());
    // Suggestions stay out until accepted.
    let r = resolve(&p, &t, Some(&off), &Review::new());
    assert!(texts(&t, &r).contains("honestly"));
    let mut rv = Review::new();
    rv.accept(&om[0].id);
    let r = resolve(&p, &t, Some(&off), &rv);
    assert_words_intact(&r, &t);
    let s = texts(&t, &r);
    assert!(!s.contains("honestly") && s.contains("loudly."));
}

// ----------------------------------------------------------- disfluency

#[test]
fn repetitions_keep_the_last_copy_but_not_emphasis() {
    let t = B::new()
        .say("I I want to go and we should we should leave now it is very very good")
        .build();
    let p = plan_of(&t, None, &EditorialConfig::natural());
    let reps = of_kind(&p, ItemKind::Repetition);
    assert_eq!(
        reps.len(),
        2,
        "{:?}",
        p.items.iter().map(|i| &i.evidence.text).collect::<Vec<_>>()
    );
    let r = resolve(&p, &t, None, &Review::new());
    assert_words_intact(&r, &t);
    let s = texts(&t, &r);
    assert!(s.starts_with("I want to go and we should leave"), "{s}");
    assert!(s.contains("very very"));
}

#[test]
fn stutters_are_removed() {
    let t = B::new()
        .say("this is th- the best way and b b but it works")
        .build();
    let p = plan_of(&t, None, &EditorialConfig::natural());
    let st = of_kind(&p, ItemKind::Stutter);
    assert_eq!(st.len(), 2, "{:?}", kinds(&p));
    let r = resolve(&p, &t, None, &Review::new());
    assert_eq!(texts(&t, &r), "this is the best way and but it works");
}

#[test]
fn self_correction_drops_the_abandoned_clause() {
    let t = B::new()
        .say("yesterday we go\u{2014} we went to the store.")
        .build();
    let p = plan_of(&t, None, &EditorialConfig::natural());
    let sc = of_kind(&p, ItemKind::SelfCorrection);
    assert_eq!(sc.len(), 1, "{:?}", kinds(&p));
    assert_eq!(sc[0].tier, Tier::Auto);
    assert_eq!(sc[0].evidence.kept_text.as_deref(), Some("we went"));
    let r = resolve(&p, &t, None, &Review::new());
    assert_words_intact(&r, &t);
    assert_eq!(texts(&t, &r), "yesterday we went to the store.");
    // A spoken cue with a substitution is only a suggestion.
    let t2 = B::new()
        .say("then go left I mean right at the light.")
        .build();
    let p2 = plan_of(&t2, None, &EditorialConfig::natural());
    let sc2 = of_kind(&p2, ItemKind::SelfCorrection);
    assert_eq!(sc2.len(), 1, "{:?}", kinds(&p2));
    assert_eq!(sc2[0].tier, Tier::Suggest);
    let mut rv = Review::new();
    rv.accept(&sc2[0].id);
    assert_eq!(
        texts(&t2, &resolve(&p2, &t2, None, &rv)),
        "then go right at the light."
    );
}

#[test]
fn false_start_needs_a_complete_replacement() {
    let t = B::new()
        .say("So the thing is\u{2014} okay here is the real point today.")
        .build();
    let p = plan_of(&t, None, &EditorialConfig::natural());
    let fs = of_kind(&p, ItemKind::FalseStart);
    assert_eq!(fs.len(), 1, "{:?}", kinds(&p));
    assert_eq!(
        fs[0].tier,
        Tier::Suggest,
        "four abandoned words are only suggested"
    );
    assert!(texts(&t, &resolve(&p, &t, None, &Review::new())).starts_with("So the thing"));
    let mut rv = Review::new();
    rv.accept(&fs[0].id);
    let r = resolve(&p, &t, None, &rv);
    assert_words_intact(&r, &t);
    assert_eq!(texts(&t, &r), "okay here is the real point today.");
    // A short abandoned start is automatic; no replacement means no item.
    let t2 = B::new().say("Well\u{2014} here is the real point.").build();
    let p2 = plan_of(&t2, None, &EditorialConfig::natural());
    assert_eq!(of_kind(&p2, ItemKind::FalseStart)[0].tier, Tier::Auto);
    let t3 = B::new().say("So the thing is\u{2014} okay.").build();
    assert!(of_kind(
        &plan_of(&t3, None, &EditorialConfig::natural()),
        ItemKind::FalseStart
    )
    .is_empty());
}

#[test]
fn fillers_come_from_the_cut_engine() {
    let t = B::new().say("So um we begin the show.").build();
    let p = plan_of(&t, None, &EditorialConfig::natural());
    let f = of_kind(&p, ItemKind::Filler);
    assert_eq!(f.len(), 1);
    assert_eq!(
        texts(&t, &resolve(&p, &t, None, &Review::new())),
        "So we begin the show."
    );
    assert_eq!(
        texts(&t, &resolve(&p, &t, None, &Review::reject_all(&p))),
        "So um we begin the show."
    );
}

// -------------------------------------------------------------- retakes

const TAKE_A: &str = "So today I want to talk about editing.";
const TAKE_B: &str = "So today I want to talk about editing videos fast.";

#[test]
fn a_later_take_cut_off_mid_sentence_loses_to_the_earlier_complete_take() {
    let full = "Welcome to the channel and today we are going to learn editing.";
    let cut = "Welcome to the channel and today we are going to";
    let t = B::new().say(full).pause(800).say(cut).build();
    let p = plan_of(&t, None, &EditorialConfig::natural());
    let g = &p.take_groups[0];
    assert_eq!(g.chosen, 0, "{g:#?}");
    assert!(g.takes[0].complete && !g.takes[1].complete);
    assert!(g.takes[1].explanation.iter().any(|e| e.contains("cut off")));
    let res = resolve(&p, &t, None, &Review::new());
    assert_words_intact(&res, &t);
    // A broken later take is only suggested for cutting (#96), so default review keeps it after the winner.
    assert!(texts(&t, &res).starts_with(full), "{}", texts(&t, &res));
    // The cut engine alone would have kept the truncated take.
    let base = crate::cuts::plan_cuts(&t, None, &crate::cuts::CutConfig::natural(), fps()).unwrap();
    assert!(base.kept_words.iter().all(|&i| i >= 12));
    // With the policy off, the last take wins as before.
    let mut cfg = EditorialConfig::natural();
    cfg.retake.prefer_complete = false;
    let p2 = plan_of(&t, None, &cfg);
    assert_eq!(p2.take_groups[0].chosen, 1);
}

#[test]
fn three_takes_keep_the_best_complete_one_and_use_energy_when_known() {
    let a = "The best editing tip is to cut every pause.";
    let b = "The best editing tip is to cut every";
    let c = "The best editing tip is to cut every pause hard.";
    let t = B::new().say(a).pause(700).say(b).pause(700).say(c).build();
    // Take C is delivered much more quietly than take A.
    let env = envelope(&t, 0.01, |i| if i >= 17 { 0.2 } else { 0.5 });
    let p = plan_of(&t, Some(&env), &EditorialConfig::natural());
    let g = &p.take_groups[0];
    assert_eq!(g.takes.len(), 3);
    assert!(!g.takes[1].complete);
    assert!(g.takes[0].scores.contains_key("energy"));
    assert_ne!(g.chosen, 1);
    assert!(g.takes[g.chosen].complete);
    let res = resolve(&p, &t, Some(&env), &Review::new());
    assert_words_intact(&res, &t);
    let s = texts(&t, &res);
    // The chosen complete take is kept. A broken later take (B) is only suggested
    // for cutting (#96), so default review keeps it after a winner that precedes it.
    assert!(s == c || s.starts_with(a), "{s}");
    for pick in 0..3 {
        let mut rv = Review::new();
        rv.pick_take(&p, &g.id, pick).unwrap();
        let r = resolve(&p, &t, Some(&env), &rv);
        assert_words_intact(&r, &t);
        let want = [a, b, c][pick];
        assert_eq!(texts(&t, &r), want);
    }
}

#[test]
fn spoken_cue_removes_the_abandoned_attempt() {
    let t = B::new()
        .say("I think the first idea is weak. Let me start over.")
        .pause(500)
        .say("The first idea is strong enough.")
        .build();
    let p = plan_of(&t, None, &EditorialConfig::natural());
    let m = of_kind(&p, ItemKind::MarkedRetake);
    assert_eq!(m.len(), 1, "{:?}", kinds(&p));
    assert_eq!(m[0].tier, Tier::Auto);
    let res = resolve(&p, &t, None, &Review::new());
    assert_words_intact(&res, &t);
    assert_eq!(texts(&t, &res), "The first idea is strong enough.");
    // A cue with no similar restart is a suggestion.
    let t2 = B::new()
        .say("Prices are going up this year. Scratch that.")
        .pause(500)
        .say("Completely different topic follows now.")
        .build();
    let p2 = plan_of(&t2, None, &EditorialConfig::natural());
    assert_eq!(of_kind(&p2, ItemKind::MarkedRetake)[0].tier, Tier::Suggest);
}

// ---------------------------------------------------------------- beats

#[test]
fn beats_label_hook_setup_point_example_and_cta() {
    let t = B::new()
        .say("Stop making this editing mistake?")
        .pause(900)
        .say("Today I am going to show you three quick tips.")
        .pause(900)
        .say("Because the key is to cut every single pause.")
        .pause(900)
        .say("For example imagine a video full of long pauses.")
        .pause(900)
        .say("Subscribe and follow for more tips.")
        .build();
    let p = plan_of(&t, None, &EditorialConfig::natural());
    let labels: Vec<BeatLabel> = p.beats.iter().map(|b| b.label).collect();
    assert_eq!(
        labels,
        vec![
            BeatLabel::Hook,
            BeatLabel::Setup,
            BeatLabel::Point,
            BeatLabel::Example,
            BeatLabel::Cta
        ],
        "{:#?}",
        p.beats
    );
    assert!(p.beats[0].hook_score.is_some_and(|s| s > 0.6));
    assert!(p.beats[4].cues.iter().any(|c| c == "subscribe"));
    assert!(p
        .beats
        .windows(2)
        .all(|w| w[0].word_range[1] == w[1].word_range[0]));
    // Items point at the beat they fall in.
    assert!(p.items.iter().all(|i| i.beat_id.is_some()));
}

// ------------------------------------------------------ plan mechanics

#[test]
fn review_overrides_defaults_and_outcomes_map_to_actions() {
    let t = B::new()
        .say("So um we begin and and then we go")
        .pause(1500)
        .say("home.")
        .build();
    let p = plan_of(&t, None, &EditorialConfig::natural());
    let f = of_kind(&p, ItemKind::Filler)[0].id.clone();
    let rep = of_kind(&p, ItemKind::Repetition)[0].id.clone();
    let mut rv = Review::new();
    rv.reject(&f);
    let res = resolve(&p, &t, None, &rv);
    let s = texts(&t, &res);
    assert!(s.contains("um") && !s.contains("and and"), "{s}");
    for o in &res.outcomes {
        if o.item_id == f {
            assert!(!o.accepted && o.removed_ms == 0);
        }
        if o.item_id == rep {
            assert!(o.accepted && o.removed_ms > 0 && !o.action_indices.is_empty());
        }
        assert!(o
            .action_indices
            .iter()
            .all(|&i| i < res.group.actions.len()));
    }
    assert!(Review::new().reject("nope").clone().accept("nope").clone() != Review::new());
    let mut bad = Review::new();
    bad.accept("ed-missing");
    assert_eq!(
        p.resolve(&t, None, &bad, &RealiseOptions::default())
            .unwrap_err(),
        EditorialError::UnknownItem("ed-missing".into())
    );
    let mut other = t.clone();
    other.source_blake3 = "ab".repeat(32);
    assert!(matches!(
        p.resolve(&other, None, &Review::new(), &RealiseOptions::default()),
        Err(EditorialError::SourceMismatch { .. })
    ));
}

#[test]
fn covered_items_fold_into_the_item_that_removes_their_words() {
    let t = B::new()
        .say(TAKE_A)
        .pause(700)
        .say("So um today I want to talk about editing videos fast.")
        .build();
    let p = plan_of(&t, None, &EditorialConfig::natural());
    assert!(
        of_kind(&p, ItemKind::Filler)
            .iter()
            .all(|f| f.covered_by.is_some()),
        "{:#?}",
        p.items
            .iter()
            .map(|i| (i.kind, &i.evidence.text, &i.covered_by))
            .collect::<Vec<_>>()
    );
}

#[test]
fn analysis_is_deterministic_and_round_trips() {
    let t = B::new()
        .say("So um I I think we go\u{2014} we went home.")
        .pause(900)
        .say(TAKE_A)
        .pause(700)
        .say(TAKE_B)
        .build();
    let cfg = EditorialConfig::tight();
    let a = plan_of(&t, None, &cfg);
    let b = plan_of(&t, None, &cfg);
    assert_eq!(a, b);
    assert_eq!(a.to_json(), b.to_json());
    let back: EditorialPlan = serde_json::from_slice(&a.to_json()).unwrap();
    assert_eq!(back, a);
    let ids: std::collections::BTreeSet<_> = a.items.iter().map(|i| &i.id).collect();
    assert_eq!(ids.len(), a.items.len());
    assert!(a.items.windows(2).all(|w| w[0].start_ms <= w[1].start_ms));
    for kind in [
        ItemKind::Silence,
        ItemKind::Filler,
        ItemKind::Repetition,
        ItemKind::SelfCorrection,
        ItemKind::Retake,
    ] {
        assert!(
            kinds(&a).contains(&kind),
            "{kind:?} missing: {:?}",
            kinds(&a)
        );
    }
    let r = resolve(&a, &t, None, &Review::new());
    assert_words_intact(&r, &t);
    assert!(r.time_map.output_frames() > 0);
}

// -------------------------------------------------------------- presets

#[test]
fn punchy_is_tighter_than_tight_and_trims_every_breath() {
    let t = B::new()
        .say("This is a pretty normal sentence")
        .pause(450)
        .say("with a pause in it")
        .pause(450)
        .say("and another one.")
        .build();
    let tight = resolve(
        &plan_of(&t, None, &EditorialConfig::tight()),
        &t,
        None,
        &Review::new(),
    );
    let punchy = resolve(
        &plan_of(&t, None, &EditorialConfig::punchy()),
        &t,
        None,
        &Review::new(),
    );
    assert_words_intact(&punchy, &t);
    assert!(
        kept_ms(&punchy) < kept_ms(&tight),
        "{} !< {}",
        kept_ms(&punchy),
        kept_ms(&tight)
    );
    let c = EditorialConfig::for_preset(Preset::Punchy);
    assert_eq!(c.cuts.preset, Preset::Punchy);
    assert!(c.cuts.max_pause_ms < EditorialConfig::tight().cuts.max_pause_ms);
    assert!(c.cuts.lead_in_ms < EditorialConfig::tight().cuts.lead_in_ms);
    assert!(c.dead_air.breath_min_ms < EditorialConfig::tight().dead_air.breath_min_ms);
    // A 180 ms breath: ignored by tight, auto-removed by punchy.
    let t = B::new()
        .say("This is the first part of it")
        .pause(900)
        .say("and this is the second.")
        .build();
    let gs = t.words[6].end_ms;
    let mut env = envelope(&t, 0.01, |_| 0.5);
    for f in ((gs + 300) / 10)..((gs + 490) / 10) {
        env[f as usize] = 0.15;
    }
    assert!(of_kind(
        &plan_of(&t, Some(&env), &EditorialConfig::tight()),
        ItemKind::Breath
    )
    .is_empty());
    let pb = plan_of(&t, Some(&env), &EditorialConfig::punchy());
    let b = of_kind(&pb, ItemKind::Breath);
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].tier, Tier::Auto);
}

#[test]
fn nothing_to_keep_is_an_error() {
    let t = B::new().build();
    assert!(analyze(&t, None, &EditorialConfig::tight(), fps()).is_err());
}
