use rightkit_timeline::{
    Action, AudioRole, Fps, SourceId, SourceInfo, Ticks, TimelineStore, VideoStreamInfo,
};
use rightkit_transcript::{EndSource, ModelRef, Span, Transcript, Word, LEGACY_SCHEMA as SCHEMA};

use crate::cuts::*;

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

/// VAD = runs of words separated by less than 150 ms.
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

fn kept_ms(p: &CutPlan) -> Vec<(i64, i64)> {
    p.kept.iter().map(|k| (k.start_ms(), k.end_ms())).collect()
}

fn total_ms(p: &CutPlan) -> i64 {
    kept_ms(p).iter().map(|(a, b)| b - a).sum()
}

/// Every surviving word sits whole inside one kept range; every removed
/// word lies entirely outside all kept ranges.
fn assert_words_intact(p: &CutPlan, t: &Transcript) {
    for (i, word) in t.words.iter().enumerate() {
        let (s, e) = (word.start_ms as i64, word.end_ms as i64);
        let inside = kept_ms(p).iter().any(|&(a, b)| s >= a && e <= b);
        let overlaps = kept_ms(p).iter().any(|&(a, b)| s < b && e > a);
        if p.kept_words.contains(&i) {
            assert!(
                inside,
                "kept word {:?} not whole in {:?}",
                word.text,
                kept_ms(p)
            );
        } else {
            assert!(
                !overlaps,
                "removed word {:?} leaks into {:?}",
                word.text,
                kept_ms(p)
            );
        }
    }
}

fn pause_transcript() -> Transcript {
    tr(
        vec![
            w("Hello", 0, 400),
            w("there", 450, 800),
            w("friends.", 2300, 2900),
        ],
        3500,
    )
}

#[test]
fn tight_cuts_a_pause_natural_keeps() {
    let t = tr(vec![w("one", 0, 300), w("two", 600, 900)], 900);
    let tight = plan_cuts(&t, None, &CutConfig::tight(), fps()).unwrap();
    let natural = plan_cuts(&t, None, &CutConfig::natural(), fps()).unwrap();
    assert_eq!(tight.kept.len(), 2);
    assert_eq!(natural.kept.len(), 1);
    assert_words_intact(&tight, &t);
    let gap_kept = total_ms(&tight) - 600;
    assert!((gap_kept - 200).abs() <= 70, "{gap_kept}");
}

#[test]
fn sentence_final_pause_is_kept_longer() {
    let mid = tr(vec![w("alpha", 0, 300), w("beta", 1300, 1600)], 1600);
    let end = tr(vec![w("alpha.", 0, 300), w("beta", 1300, 1600)], 1600);
    let cfg = CutConfig::natural();
    let a = total_ms(&plan_cuts(&mid, None, &cfg, fps()).unwrap());
    let b = total_ms(&plan_cuts(&end, None, &cfg, fps()).unwrap());
    assert!((b - a - 200).abs() <= 70, "{a} {b}");
}

#[test]
fn removes_plain_and_isolated_context_fillers() {
    let t = tr(
        vec![
            w("So,", 0, 200),
            w("um,", 500, 700),
            w("I", 1000, 1100),
            w("think", 1150, 1500),
            w("you", 1900, 2000),
            w("know,", 2010, 2200),
            w("we", 2700, 2900),
            w("should", 2950, 3300),
            // not isolated: stays
            w("you", 3350, 3450),
            w("know", 3460, 3600),
            w("what", 3650, 3900),
        ],
        4000,
    );
    let p = plan_cuts(&t, None, &CutConfig::tight(), fps()).unwrap();
    assert_words_intact(&p, &t);
    let removed: Vec<&str> = p
        .decisions
        .iter()
        .filter(|d| d.kind == RemovalReason::Filler)
        .map(|d| d.text.as_str())
        .collect();
    assert_eq!(removed, vec!["um,", "you know,"]);
    assert!(p
        .decisions
        .iter()
        .any(|d| d.detail.contains("isolated filler")));
    assert!(p.kept_words.contains(&8) && p.kept_words.contains(&9));
}

#[test]
fn filler_at_start_end_and_back_to_back() {
    let t = tr(
        vec![
            w("Um", 300, 500),
            w("hello", 900, 1300),
            w("there.", 1350, 1700),
            w("so", 2000, 2200),
            w("um", 2300, 2500),
            w("uh", 2520, 2700),
            w("yeah", 3000, 3300),
            w("Uh.", 3800, 4000),
        ],
        4500,
    );
    let p = plan_cuts(&t, None, &CutConfig::tight(), fps()).unwrap();
    assert_words_intact(&p, &t);
    assert_eq!(p.kept_words, vec![1, 2, 3, 6]);
    // head trimmed to the lead-in, never back into "Um"
    assert!(p.kept[0].start_ms() >= 500 && p.kept[0].start_ms() <= 900);
    // tail ends before the trailing "Uh." starts
    assert!(p.kept.last().unwrap().end_ms() <= 3800);
    // back-to-back fillers form one removal with filler reason
    let r = p
        .removed
        .iter()
        .find(|r| r.source_start.to_units(1000) >= 2200 && r.source_start.to_units(1000) < 2500)
        .unwrap();
    assert_eq!(r.reason, RemovalReason::Filler);
    assert!(r.decisions.len() >= 2);
}

#[test]
fn very_short_words_survive() {
    let t = tr(
        vec![w("I", 0, 20), w("am", 60, 300), w("here.", 350, 700)],
        700,
    );
    let p = plan_cuts(&t, None, &CutConfig::tight(), fps()).unwrap();
    assert_words_intact(&p, &t);
    assert_eq!(p.kept_words.len(), 3);
    assert!(p.kept.iter().all(|k| k.source_out > k.source_in));
}

#[test]
fn repeated_sentence_with_debris_keeps_only_the_last() {
    let sentence = ["So", "today", "we", "talk", "about", "cuts."];
    let mut words = Vec::new();
    let mut t0 = 0u64;
    for x in sentence {
        words.push(w(x, t0, t0 + 250));
        t0 += 300;
    }
    t0 += 900;
    words.push(w("Sorry.", t0, t0 + 400));
    t0 += 1600;
    for x in sentence {
        words.push(w(x, t0, t0 + 250));
        t0 += 300;
    }
    let t = tr(words, t0 + 500);
    let p = plan_cuts(&t, None, &CutConfig::natural(), fps()).unwrap();
    assert_words_intact(&p, &t);
    assert_eq!(p.kept_words, (7..13).collect::<Vec<_>>());
    let d = p
        .decisions
        .iter()
        .find(|d| d.kind == RemovalReason::Retake)
        .unwrap();
    assert_eq!(d.text, "So today we talk about cuts. Sorry.");
    assert_eq!(d.kept_text.as_deref(), Some("So today we talk about cuts."));
    assert_eq!(p.removed[0].source_start, Ticks::ZERO);
}

#[test]
fn different_sentences_are_not_retakes() {
    let t = tr(
        vec![
            w("Today", 0, 300),
            w("we", 350, 450),
            w("cut", 500, 700),
            w("video.", 750, 1100),
            w("Tomorrow", 2200, 2600),
            w("we", 2650, 2750),
            w("render", 2800, 3100),
            w("audio.", 3150, 3500),
        ],
        3600,
    );
    let p = plan_cuts(&t, None, &CutConfig::natural(), fps()).unwrap();
    assert_eq!(p.kept_words.len(), 8);
    assert!(p.decisions.iter().all(|d| d.kind != RemovalReason::Retake));
}

#[test]
fn next_token_start_ends_are_refined_with_vad() {
    let mut words = vec![w("Hello", 0, 1500), w("world.", 1500, 2000)];
    for x in &mut words {
        x.end_source = EndSource::NextTokenStart;
    }
    let mut t = tr(words, 2600);
    t.vad = vec![
        Span {
            start_ms: 0,
            end_ms: 400,
        },
        Span {
            start_ms: 1500,
            end_ms: 1900,
        },
    ];
    let p = plan_cuts(&t, None, &CutConfig::natural(), fps()).unwrap();
    assert_eq!(p.kept.len(), 2, "{:?}", kept_ms(&p));
    // pause 1100 -> 500: output ~ 400 + 500 + 400 + tail 250
    assert!((total_ms(&p) - 1550).abs() <= 105, "{}", total_ms(&p));
    // "Hello" stays whole even though its reported end is 1500
    assert!(p.kept[0].end_ms() >= 400);
    // without VAD or envelope the approximate ends are trusted: nothing to cut
    let mut blind = t.clone();
    blind.vad.clear();
    let q = plan_cuts(&blind, None, &CutConfig::natural(), fps()).unwrap();
    assert_eq!(q.kept.len(), 1);
}

#[test]
fn next_token_start_refined_with_envelope_only() {
    let mut words = vec![w("Hello", 0, 1500), w("world.", 1500, 2000)];
    for x in &mut words {
        x.end_source = EndSource::NextTokenStart;
    }
    let mut t = tr(words, 2600);
    t.vad.clear();
    let mut env = vec![0.01f32; 260];
    for v in env.iter_mut().take(40) {
        *v = 0.5;
    }
    for v in env.iter_mut().skip(150).take(40) {
        *v = 0.5;
    }
    let p = plan_cuts(&t, Some(&env), &CutConfig::natural(), fps()).unwrap();
    assert_eq!(p.kept.len(), 2);
    assert!(p.kept[0].end_ms() >= 400);
}

#[test]
fn cut_points_snap_to_the_quietest_envelope_frame() {
    let t = tr(vec![w("Hello", 0, 400), w("world", 1900, 2300)], 2300);
    let mut env = vec![0.05f32; 230];
    for v in env.iter_mut().take(40) {
        *v = 0.5;
    }
    for v in env.iter_mut().skip(190) {
        *v = 0.5;
    }
    env[56] = 0.0; // 560..570 ms
    let mut cfg = CutConfig::natural();
    cfg.snap_window_ms = 120;
    let p = plan_cuts(&t, Some(&env), &cfg, fps()).unwrap();
    let e = p.kept[0].end_ms();
    assert!((555..=580).contains(&e), "{e}");
    let plain = plan_cuts(&t, None, &cfg, fps()).unwrap();
    assert!(plain.kept[0].end_ms() > 600);
}

#[test]
fn manual_removal_snaps_to_words() {
    let t = tr(
        vec![
            w("keep", 0, 300),
            w("drop", 350, 650),
            w("this", 700, 900),
            w("keep", 950, 1300),
        ],
        1400,
    );
    let mut cfg = CutConfig::tight();
    cfg.manual_removals.push(ManualRemoval {
        start_ms: 400,
        end_ms: 900,
    });
    let p = plan_cuts(&t, None, &cfg, fps()).unwrap();
    assert_eq!(p.kept_words, vec![0, 3]);
    assert!(p.removed.iter().any(|r| r.reason == RemovalReason::Manual));
    assert_words_intact(&p, &t);
}

#[test]
fn all_removed_is_an_error_and_plans_are_deterministic() {
    let t = tr(vec![w("um", 100, 300)], 600);
    assert_eq!(
        plan_cuts(&t, None, &CutConfig::tight(), fps()).unwrap_err(),
        CutError::NothingKept
    );
    let t = pause_transcript();
    let a = plan_cuts(&t, None, &CutConfig::natural(), fps()).unwrap();
    let b = plan_cuts(&t, None, &CutConfig::natural(), fps()).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.to_json(), b.to_json());
}

fn source(duration_ms: u64) -> SourceInfo {
    SourceInfo {
        id: SourceId::new(HEX).unwrap(),
        duration: Ticks::from_millis(duration_ms as i64),
        video: Some(VideoStreamInfo {
            width: 1920,
            height: 1080,
            duration: None,
        }),
        has_audio: true,
    }
}

#[test]
fn undo_group_refuses_when_not_latest() {
    let t = pause_transcript();
    let p = plan_cuts(&t, None, &CutConfig::tight(), fps()).unwrap();
    let mut store = TimelineStore::new(fps());
    store
        .apply(Action::RegisterSource {
            source: source(t.duration_ms),
        })
        .unwrap();
    let applied = apply_group(&mut store, &p.group(&RealiseOptions::default())).unwrap();
    store
        .apply(Action::AddAudioTrack {
            track_id: "A2".into(),
            role: AudioRole::Music,
        })
        .unwrap();
    assert!(undo_group(&mut store, &applied).is_err());
}

#[test]
fn time_map_maps_words_into_output_time() {
    let t = pause_transcript();
    let p = plan_cuts(&t, None, &CutConfig::natural(), fps()).unwrap();
    let map = p.time_map();
    // Round-trip and removed-time semantics sit under the delivered word-edge
    // guarantees proven by the engine-level e2e; keep the unit check for the
    // API surface an e2e cannot call directly.
    let out = map.source_ms_to_output_ms(2400).unwrap();
    assert_eq!(map.output_ms_to_source_ms(out), Some(2400));
    assert_eq!(map.source_ms_to_output_ms(1200), None);
    assert_eq!(
        map.output_duration(),
        fps().frame_to_ticks(map.output_frames())
    );
}
