//! Synthetic long-transcript checks for the shorts finder and the short builder.

use rightkit_shorts::score::*;
use rightkit_shorts::*;
use rightkit_timeline::{Fps, SourceId, SourceInfo, Ticks, VideoStreamInfo};
use rightkit_transcript::{EndSource, ModelRef, Span, Transcript, Word, LEGACY_SCHEMA as SCHEMA};

const HEX: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

fn word(text: &str, s: u64, e: u64) -> Word {
    Word {
        text: text.into(),
        start_ms: s,
        end_ms: e,
        confidence: Some(0.93),
        end_source: EndSource::Model,
    }
}

fn transcript(words: Vec<Word>) -> Transcript {
    let duration_ms = words.last().map_or(1000, |w| w.end_ms + 1500);
    let mut vad: Vec<Span> = Vec::new();
    for w in &words {
        match vad.last_mut() {
            Some(l) if w.start_ms - l.end_ms < 150 => l.end_ms = w.end_ms,
            _ => vad.push(Span {
                start_ms: w.start_ms,
                end_ms: w.end_ms,
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
            id: "synthetic".into(),
            revision: "1".into(),
            backend: "synthetic".into(),
        },
    }
}

/// Deterministic pseudo-random stream.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

const SUBJECTS: &[&str] = &[
    "the garden",
    "my camera",
    "this editor",
    "our budget",
    "the morning routine",
    "that old laptop",
    "your thumbnail",
    "the lighting",
    "a good microphone",
    "the schedule",
];
const VERBS: &[&str] = &[
    "needs", "changes", "improves", "hides", "shows", "costs", "protects", "teaches",
];
const OBJECTS: &[&str] = &[
    "more patience",
    "a clear plan",
    "fresh ideas",
    "better habits",
    "steady practice",
    "honest feedback",
    "simple tools",
    "careful notes",
];

/// ~`minutes` of speech: unique-ish sentences at ~2.7 words/s, 250-900 ms pauses, a
/// 5 s dead gap every 45 sentences, and a few planted sentences.
fn long_transcript(minutes: u64) -> Transcript {
    let mut rng = Lcg(7);
    let mut words = Vec::new();
    let mut t = 500u64;
    let mut idx = 0u64;
    while t < minutes * 60_000 {
        idx += 1;
        let sentence = match idx {
            10 => {
                "Did you know that nobody tells you the biggest mistake with lighting?".to_string()
            }
            11 => "The truth is that tested results show 40 percent better retention.".to_string(),
            12 => "That is why a simple routine wins in the end.".to_string(),
            _ => format!(
                "{} {} {} and {} {} {}, which matters for episode {idx}.",
                ["Today", "Often", "Honestly", "Usually"][rng.next(4) as usize],
                SUBJECTS[rng.next(SUBJECTS.len() as u64) as usize],
                VERBS[rng.next(VERBS.len() as u64) as usize],
                SUBJECTS[rng.next(SUBJECTS.len() as u64) as usize],
                VERBS[rng.next(VERBS.len() as u64) as usize],
                OBJECTS[rng.next(OBJECTS.len() as u64) as usize],
            ),
        };
        let n_words = sentence.split_whitespace().count() as u64;
        for w in sentence.split_whitespace() {
            let len = 250 + 22 * w.len() as u64 + rng.next(60);
            words.push(word(w, t, t + len));
            t += len + 40;
        }
        let _ = n_words;
        t += 250 + rng.next(650);
        if idx.is_multiple_of(45) {
            t += 5_000;
        }
    }
    transcript(words)
}

fn run(tr: &Transcript, count: usize) -> Proposals {
    propose_shorts(&ProposeInput::new(tr), &ShortsProfile::default(), count).unwrap()
}

#[test]
fn proposals_are_word_aligned_scored_and_unselected() {
    let tr = long_transcript(10);
    assert!(tr.words.len() > 1000);
    let out = run(&tr, 8);
    assert_eq!(out.status, "proposed");
    assert_eq!(out.selected_id, None);
    assert!(out.windows_considered > 100, "{}", out.windows_considered);
    assert!(!out.proposals.is_empty() && out.proposals.len() <= 8);
    let profile = ShortsProfile::default();
    let primary = profile.primary();
    for (i, p) in out.proposals.iter().enumerate() {
        assert_eq!(p.rank, i + 1);
        // Range edges are exactly word edges, and the indices name those words.
        assert_eq!(p.start_ms, tr.words[p.first_word].start_ms);
        assert_eq!(p.end_ms, tr.words[p.last_word].end_ms);
        // Neither the range nor the padded clip range lands inside any word.
        for w in &tr.words {
            for edge in [p.start_ms, p.end_ms, p.clip_start_ms, p.clip_end_ms] {
                assert!(
                    !(w.start_ms < edge && edge < w.end_ms),
                    "edge {edge} is mid-word ({}..{})",
                    w.start_ms,
                    w.end_ms
                );
            }
        }
        assert!(p.clip_start_ms <= p.start_ms && p.clip_end_ms >= p.end_ms);
        let d = p.duration_ms as i64;
        assert!((primary.min_duration_ms..=primary.max_duration_ms).contains(&d));
        assert!(p.standalone.passed);
        assert!(p.scores.composite > 0.0 && p.scores.composite <= 1.0);
        assert!(!p.title.is_empty() && p.title.chars().count() <= profile.title.max_chars + 1);
        assert!(p.preview.poster_ms >= p.start_ms && p.preview.poster_ms <= p.end_ms);
        assert_eq!(p.platform_fit.len(), profile.platforms.len());
        assert!(p.id.starts_with("short-"));
    }
    // Ids unique; no heavy overlap among proposals.
    for (i, a) in out.proposals.iter().enumerate() {
        for b in &out.proposals[i + 1..] {
            assert_ne!(a.id, b.id);
            let ov = a
                .end_ms
                .min(b.end_ms)
                .saturating_sub(a.start_ms.max(b.start_ms));
            assert!(
                ov as f64 / a.duration_ms.min(b.duration_ms) as f64 <= 0.3,
                "{} overlaps {}",
                a.id,
                b.id
            );
        }
    }
    assert!(
        out.suppressed_overlap > 0,
        "overlap suppression never fired"
    );
}

#[test]
fn output_is_deterministic_and_round_trips_json() {
    let tr = long_transcript(4);
    let a = run(&tr, 5);
    let b = run(&tr, 5);
    assert_eq!(a, b);
    let json = serde_json::to_string_pretty(&a).unwrap();
    assert_eq!(serde_json::from_str::<Proposals>(&json).unwrap(), a);
}

#[test]
fn planted_hook_and_proof_sentences_lead_their_window() {
    let tr = long_transcript(3);
    let out = run(&tr, 6);
    let top = &out.proposals[0];
    // Sentences 10-12 are the planted hook / proof / payoff run.
    assert!(
        top.transcript.contains("Did you know") || top.transcript.contains("tested results"),
        "top proposal missed the planted hook/proof: {}",
        top.transcript
    );
    assert!(top.scores.hook.value >= 0.8, "{:?}", top.scores.hook);
    assert!(top.scores.proof.value >= 0.5, "{:?}", top.scores.proof);
    assert_eq!(top.scores.hook.confidence, Confidence::High);
}

#[test]
fn dangling_openers_are_rejected_not_proposed() {
    let tr = long_transcript(2);
    let out = run(&tr, 20);
    for p in &out.proposals {
        let first = p
            .transcript
            .split_whitespace()
            .next()
            .unwrap()
            .to_lowercase();
        assert!(!["and", "but", "so", "that", "it", "they"].contains(&first.trim_matches(',')));
    }
    // A transcript whose only usable window opens on "and" yields nothing.
    let mut words = Vec::new();
    let mut t = 0;
    for w in "And then the whole thing fell apart quickly while everyone watched in silence for a long time without saying a single word at all."
        .split_whitespace()
    {
        words.push(word(w, t, t + 500));
        t += 540;
    }
    let tr = transcript(words);
    let out = run(&tr, 5);
    assert!(out.proposals.is_empty());
    assert!(out
        .rejected
        .iter()
        .any(|r| r.stage == "standalone_context" && r.reason.contains("\"and\"")));
}

#[test]
fn a_long_pause_ends_the_window() {
    let mut words = Vec::new();
    let mut t = 0u64;
    let say = |words: &mut Vec<Word>, t: &mut u64, text: &str| {
        for w in text.split_whitespace() {
            words.push(word(w, *t, *t + 450));
            *t += 480;
        }
    };
    say(&mut words, &mut t, "Here is the first complete thought about the garden, and it runs for a good while before ending properly.");
    t += 4_000;
    say(&mut words, &mut t, "Then the second thought about cameras begins after a long gap, and it also runs for quite some time here.");
    let tr = transcript(words);
    let out = run(&tr, 10);
    assert!(!out.proposals.is_empty());
    for p in &out.proposals {
        let gap_inside = (p.first_word..p.last_word)
            .any(|k| tr.words[k + 1].start_ms - tr.words[k].end_ms > 3_000);
        assert!(!gap_inside, "window spans the 4 s pause: {}", p.transcript);
    }
}

#[test]
fn duplicate_takes_do_not_fill_every_slot() {
    // The same paragraph said three times: the diversity stage must keep one.
    let para = "Never skip the warm up routine because it protects your voice and your focus for hours, which is why it matters so much every single day.";
    let mut words = Vec::new();
    let mut t = 0u64;
    for _ in 0..3 {
        for w in para.split_whitespace() {
            words.push(word(w, t, t + 420));
            t += 450;
        }
        t += 2_500;
    }
    let tr = transcript(words);
    let mut profile = ShortsProfile::default();
    profile.dedupe.max_overlap_fraction = 1.0; // isolate clustering from overlap suppression
    let out = propose_shorts(&ProposeInput::new(&tr), &profile, 5).unwrap();
    let clusters: std::collections::BTreeSet<_> = out
        .proposals
        .iter()
        .map(|p| p.diversity_cluster.clone())
        .collect();
    assert!(!out.proposals.is_empty());
    assert!(
        clusters.len() * 2 <= out.proposals.len() + 1,
        "near-duplicates were not clustered: {} clusters for {} proposals",
        clusters.len(),
        out.proposals.len()
    );
}

#[test]
fn stage_scores_follow_the_old_formulas() {
    let p = ShortsProfile::default();
    // Stage 4, duration fit: 0 outside the hard window, 1 inside ideal, linear between.
    let v = &p.platforms[0];
    assert_eq!(score_duration_fit(4_000, v).value, 0.0);
    assert_eq!(score_duration_fit(95_000, v).value, 0.0);
    assert_eq!(score_duration_fit(30_000, v).value, 1.0);
    assert!((score_duration_fit(10_000, v).value - 0.5).abs() < 1e-9);
    assert!((score_duration_fit(67_500, v).value - 0.5).abs() < 1e-9);
    // Stage 2.
    assert!(!validate_standalone("But that was the reason.", &p).passed);
    assert!(validate_standalone("Here is the reason.", &p).passed);
    // Stage 3.
    let hook = score_hook("Stop making this mistake?", false, &p);
    assert!((hook.value - 0.8).abs() < 1e-9);
    assert!((score_hook("Stop.", true, &p).value - 0.6).abs() < 1e-9);
    assert!((score_payoff("It works. That's why it matters.", &p).value - 1.0).abs() < 1e-9);
    assert!((score_payoff("It works and", &p).value - 0.0).abs() < 1e-9);
    assert!((score_proof("We tested 12 cases.", &p).value - 1.0).abs() < 1e-9);
    assert_eq!(score_proof("Nothing here.", &p).confidence, Confidence::Low);
    // Word-boundary cue matching: "so" must not match inside "also".
    assert_eq!(score_hook("also never", false, &p).value, 0.4);
    let w = word("hello", 0, 100);
    let words: Vec<&Word> = (0..20).map(|_| &w).collect();
    let v_ok = score_value(&words, 0, 8_000, &p); // 2.5 wps, inside the band
    let v_fast = score_value(&words, 0, 2_000, &p); // 10 wps
    assert!(v_ok.value > v_fast.value);
    // Stage 6.
    assert_eq!(score_platform_fit(&["9:16".into()]).value, 1.0);
    assert_eq!(score_platform_fit(&["16:9".into()]).value, 0.6);
    assert_eq!(score_platform_fit(&[]).confidence, Confidence::Low);
    assert_eq!(score_brand_fit("anything", &p).confidence, Confidence::Low);
    let mut banned = p.clone();
    banned.brand_banned_terms = vec!["garden".into()];
    assert_eq!(score_brand_fit("the garden", &banned).value, 0.0);
    // Composite is the weighted mean.
    let s = |v: f64| Score {
        value: v,
        confidence: Confidence::High,
        rationale: String::new(),
    };
    let b = ScoreBreakdown {
        hook: s(1.0),
        payoff: s(1.0),
        proof: s(1.0),
        value: s(1.0),
        duration_fit: s(1.0),
        visual_support: s(1.0),
        platform_fit: s(1.0),
        brand_fit: s(0.0),
        composite: 0.0,
    };
    assert!((composite_score(&b, &p.weights) - 0.95).abs() < 1e-9);
}

#[test]
fn visual_support_reads_a_track_and_stays_neutral_without_one() {
    let p = ShortsProfile::default();
    assert_eq!(score_visual_support(None, &p.visual_support).value, 0.5);
    assert_eq!(
        score_visual_support(None, &p.visual_support).confidence,
        Confidence::Low
    );
    assert_eq!(
        score_visual_support(Some(&[]), &p.visual_support).value,
        0.5
    );
    let pts: Vec<VisualPoint> = (0..10)
        .map(|i| VisualPoint {
            source_ms: i * 500,
            center_x: 0.4 + i as f64 * 0.02,
            center_y: 0.5,
            confidence: 0.9,
            gap: false,
            shot_boundary: i == 0,
        })
        .collect();
    let refs: Vec<&VisualPoint> = pts.iter().collect();
    let s = score_visual_support(Some(&refs), &p.visual_support);
    assert!(s.value > 0.5, "{s:?}");
    assert_eq!(s.confidence, Confidence::High);
}

#[test]
fn visual_track_changes_the_proposal_score() {
    let tr = long_transcript(2);
    let base = run(&tr, 3);
    let pts: Vec<VisualPoint> = (0..2_400)
        .map(|i| VisualPoint {
            source_ms: i * 50,
            center_x: 0.5,
            center_y: 0.5,
            confidence: 0.95,
            gap: false,
            shot_boundary: false,
        })
        .collect();
    let mut input = ProposeInput::new(&tr);
    input.visual = Some(&pts);
    let with = propose_shorts(&input, &ShortsProfile::default(), 3).unwrap();
    assert_eq!(base.proposals[0].scores.visual_support.value, 0.5);
    assert_ne!(with.proposals[0].scores.visual_support.value, 0.5);
}

#[test]
fn profile_json_round_trips_and_rejects_bad_windows() {
    let p = ShortsProfile::default();
    assert_eq!(ShortsProfile::from_json(&p.to_json()).unwrap(), p);
    let mut bad = p.clone();
    bad.platforms[0].ideal_min_ms = 1;
    assert!(ShortsProfile::from_json(&bad.to_json()).is_err());
}

fn source_info(duration_ms: i64) -> SourceInfo {
    SourceInfo {
        id: SourceId::new(HEX).unwrap(),
        duration: Ticks::from_millis(duration_ms),
        video: Some(VideoStreamInfo {
            width: 1920,
            height: 1080,
            duration: None,
        }),
        has_audio: true,
    }
}

#[test]
fn no_cut_short_keeps_the_padded_range_whole() {
    let tr = long_transcript(2);
    let out = run(&tr, 2);
    let p = &out.proposals[0];
    let fps = Fps::new(30, 1).unwrap();
    let opts = ShortOptions {
        cut: None,
        captions: false,
        ..ShortOptions::default()
    };
    let plan = build_short(
        p,
        &tr,
        &source_info(tr.duration_ms as i64),
        fps,
        &opts,
        None,
    )
    .unwrap();
    assert_eq!(plan.cut_plan.kept.len(), 1);
    let k = plan.cut_plan.kept[0];
    assert!(k.start_ms() as u64 <= p.start_ms && k.end_ms() as u64 >= p.end_ms);
    assert!(!plan
        .actions
        .iter()
        .any(|a| matches!(a, rightkit_timeline::Action::AddTitle { .. })));
}

#[test]
fn a_stale_proposal_is_refused() {
    let tr = long_transcript(2);
    let out = run(&tr, 1);
    let mut p = out.proposals[0].clone();
    p.start_ms += 10;
    let fps = Fps::new(30, 1).unwrap();
    let err = build_short(
        &p,
        &tr,
        &source_info(tr.duration_ms as i64),
        fps,
        &ShortOptions::default(),
        None,
    );
    assert!(matches!(err, Err(ShortsError::Mismatch(_))));
}
