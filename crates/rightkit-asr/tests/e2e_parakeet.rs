//! End-to-end Parakeet decoding with real audio and real models.
//!
//! Gates (a missing asset prints a SKIP line and the test passes; set
//! `RIGHTKIT_REQUIRE_ASSETS=1` to make a missing asset fail). Exact
//! files each gate needs:
//!
//! CoreML (macOS), `RIGHTKIT_PARAKEET_COREML_DIR` or a local registry dir that
//! holds, for the shipped `parakeet-tdt-v3` export (~1.1 GB):
//!   `MelSpectrogram.mlmodelc`, `AudioEncoder.mlmodelc`, `TextDecoder.mlmodelc`,
//!   `Joint.mlmodelc` (each with `coremldata.bin`, `weights/weight.bin`),
//!   `pipeline.json`, `vocab.txt`.
//! CoreML `.mlpackage` compile path, `RIGHTKIT_PARAKEET_COREML_PKG_DIR`: the same
//!   four stages as `.mlpackage`, `pipeline.json`, `vocab.txt` (ScrapeRight
//!   `tdt_w15_p6`).
//! ONNX (any OS), `RIGHTKIT_PARAKEET_ONNX_DIR`: a transducer export dir with
//!   `encoder-model.onnx` (+ `encoder-model.onnx.data` if external),
//!   `decoder_joint-model.onnx`, `nemo128.onnx` (mel), `vocab.txt`, `config.json`
//!   (parakeet-tdt-0.6b-v3 ONNX, int8 or fp32), plus an ONNX Runtime dylib via
//!   `ORT_DYLIB_PATH`. No such export exists on this machine today.
//!
//! The model-free tests exercise the shared decode contract against the real
//! audio fixtures and the real detokenizer fixture file.
#[macro_use]
mod common;

mod coreml_cache;

use rightkit_asr::longform::{self, LongFormOptions};
use rightkit_asr::parakeet_core::{assemble_pieces, DetokPolicy};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use rightkit_asr::TimestampMode;

fn wav(name: &str) -> Vec<f32> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    let mut r = hound::WavReader::open(p).unwrap();
    assert_eq!((r.spec().sample_rate, r.spec().channels), (16_000, 1));
    r.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect()
}

const SHORT: &[&str] = &["timer", "minutes", "remind", "oven", "mangoes", "bananas", "sparkling", "water"];
const LONG: &[&str] = &["healthy", "small", "epoch", "medium", "alive", "rollback", "threshold"];

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}

fn assert_keywords(text: &str, kws: &[&str]) {
    let w = words(text);
    let missing: Vec<_> = kws.iter().filter(|k| !w.iter().any(|x| x == **k)).collect();
    assert!(missing.is_empty(), "missing keywords {missing:?} in transcript: {text}");
}

#[allow(dead_code)]
fn first_existing(env: &str, defaults: &[&str]) -> Option<PathBuf> {
    std::env::var_os(env)
        .map(PathBuf::from)
        .into_iter()
        .chain(defaults.iter().map(PathBuf::from))
        .find(|p| p.exists())
}

// ---------------------------------------------------------------------------
// Model-free: shared contract on real files
// ---------------------------------------------------------------------------

#[test]
fn canonical_detok_fixture_file_contract() {
    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        pieces: Vec<String>,
        expected: String,
    }
    let raw = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/detok_cases.json"),
    )
    .unwrap();
    let cases: Vec<Case> = serde_json::from_str(&raw).unwrap();
    assert!(cases.len() >= 5, "fixture must carry the HeardRight detok cases");
    for c in cases {
        let got = assemble_pieces(&c.pieces, DetokPolicy::canonical_rust());
        assert_eq!(got.text, c.expected, "case={}", c.name);
    }
    // iOS legacy mode must not secretly add the digit-boundary repair.
    assert_eq!(
        assemble_pieces(["\u{2581}at", "6", "0"], DetokPolicy::sentencepiece_only(false)).text,
        "at60"
    );
    assert_eq!(
        assemble_pieces(["\u{2581}at", "6", "0"], DetokPolicy::canonical_rust()).text,
        "at 60"
    );
}

/// The padded-window schedule on the real 38 s fixture: windows are disjoint,
/// contiguous, cover every sample, never exceed 15 s, and every cut sits at the
/// quietest 200 ms span of the trailing 2.24 s.
#[test]
fn padded_window_schedule_cuts_real_audio_at_the_quietest_span() {
    let audio = wav("long_38s.wav");
    let opts = LongFormOptions { window_samples: 240_000, terminal_silence: false, seam: longform::SeamPolicy::DedupOverlap, word_assembly_floor_ms: Some(80), pad_policy: longform::PadPolicy::FinalSegmentOnly };
    let mut seen: Vec<(usize, usize)> = Vec::new();
    let mut cursor_pos = 0usize;
    let progress_events = std::cell::Cell::new(0usize);
    let result = longform::transcribe_padded_window(&audio, &opts, None, &|_| progress_events.set(progress_events.get() + 1), |seg| {
        // Locate the segment by pointer offset into `audio`.
        let start = (seg.as_ptr() as usize - audio.as_ptr() as usize) / 4;
        assert_eq!(start, cursor_pos, "windows must be contiguous");
        cursor_pos = start + seg.len();
        seen.push((start, seg.len()));
        Ok(rightkit_asr::parakeet_core::TranscriptionResult {
            text: format!("w{}", seen.len()),
            tokens: vec![rightkit_asr::parakeet_core::TimedToken {
                text: format!("w{}", seen.len()),
                start: 0.0,
                end: 0.5,
            }],
        })
    })
    .unwrap();
    assert!(seen.len() >= 3, "38 s must need 3 windows, got {seen:?}");
    assert_eq!(cursor_pos, audio.len(), "every sample decoded exactly once");
    assert_eq!(progress_events.get(), seen.len());
    let padding = longform::ms_to_samples(longform::PADDED_WINDOW_PADDING_MS);
    for &(start, len) in &seen[..seen.len() - 1] {
        assert!(len <= 240_000);
        let cut = start + len;
        // The cut is the midpoint of the quietest 200 ms span in the search region.
        let lo = start + 240_000 - padding;
        let hi = start + 240_000;
        assert_eq!(longform::quiet_cut_in(&audio, lo, hi), Some(cut), "cut is not the argmin span");
    }
    // Seams without overlap get a separator: "w1 w2 w3", not "w1w2w3".
    assert!(result.text.starts_with("w1 w2"), "{}", result.text);
    // Timed pieces are re-based onto the source timeline and monotonic.
    assert!(result.tokens.windows(2).all(|p| p[1].start >= p[0].start));
    assert!((result.tokens[1].start - seen[1].0 as f32 / 16_000.0).abs() < 1e-3);
}

/// CutRight contract: disjoint windows commit exactly as decoded, so a word the
/// speaker really repeated across a seam survives; the HeardRight/ScrapeRight
/// overlap eraser would delete it (and its timing).
#[test]
fn exact_seam_keeps_a_word_repeated_across_windows_that_dedup_erases() {
    use rightkit_asr::parakeet_core::{TimedToken, TranscriptionResult};
    let audio = wav("long_38s.wav");
    let run = |seam| {
        let opts = LongFormOptions { window_samples: 240_000, terminal_silence: false, seam, word_assembly_floor_ms: Some(80), pad_policy: longform::PadPolicy::FinalSegmentOnly };
        longform::transcribe_padded_window(&audio, &opts, None, &|_| {}, |_| {
            Ok(TranscriptionResult {
                text: "that".into(),
                tokens: vec![TimedToken { text: " that".into(), start: 0.1, end: 0.4 }],
            })
        })
        .unwrap()
    };
    let exact = run(longform::SeamPolicy::Exact);
    let dedup = run(longform::SeamPolicy::DedupOverlap);
    assert_eq!(exact.tokens.len(), 3, "one piece per window, none erased");
    assert_eq!(exact.text.trim(), "that that that");
    assert!(exact.tokens.windows(2).all(|p| p[1].start > p[0].start), "re-based onto the source timeline");
    assert!(dedup.tokens.len() < exact.tokens.len(), "dedup consumes the repeated seam word");
}

#[test]
fn long_form_cancellation_stops_between_windows() {
    let audio = wav("long_38s.wav");
    let opts = LongFormOptions { window_samples: 240_000, terminal_silence: false, seam: longform::SeamPolicy::DedupOverlap, word_assembly_floor_ms: Some(80), pad_policy: longform::PadPolicy::FinalSegmentOnly };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut decoded = 0usize;
    let err = longform::transcribe_padded_window(
        &audio,
        &opts,
        Some(&cancel),
        &|p| {
            if p.windows_done == 1 {
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        },
        |_| {
            decoded += 1;
            Ok(rightkit_asr::parakeet_core::TranscriptionResult { text: "x".into(), tokens: vec![] })
        },
    )
    .unwrap_err();
    assert!(matches!(err, rightkit_asr::Error::Cancelled), "{err}");
    assert_eq!(decoded, 1, "no window may start after cancellation");
}

#[test]
fn subtitle_exports_follow_the_cue_rules() {
    use rightkit_asr::parakeet_core::TimedToken;
    let t = |s: &str, a: f32, b: f32| TimedToken { text: s.into(), start: a, end: b };
    let pieces = vec![
        t(" Hello", 0.0, 0.4),
        t(",", 0.4, 0.4),
        t(" world", 0.5, 0.9),
        t(".", 0.9, 0.9),
        t(" Then", 3.5, 3.8), // > 1 s gap closes the cue
        t(" more", 3.9, 4.2),
    ];
    let ex = longform::timed_exports(&pieces);
    let srt = ex.srt.unwrap();
    assert!(srt.starts_with("1\n00:00:00,000 --> 00:00:"), "{srt}");
    assert!(srt.contains("Hello, world."), "{srt}");
    assert!(srt.contains("2\n00:00:03,500 --> 00:00:04,200\nThen more"), "{srt}");
    let vtt = ex.vtt.unwrap();
    assert!(vtt.starts_with("WEBVTT\n\n00:00:00.000 --> "), "{vtt}");
    let json: serde_json::Value = serde_json::from_str(&ex.json.unwrap()).unwrap();
    assert_eq!(json["words"].as_array().unwrap().len(), 4);
    assert_eq!(json["segments"].as_array().unwrap().len(), 2);
    assert!(longform::timed_exports(&[]).srt.is_none());
}

// ---------------------------------------------------------------------------
// CoreML (macOS)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod coreml {
    const COREML_LOOKED_AT: &str = "RIGHTKIT_PARAKEET_COREML_DIR, or the HeardRightNext or Orthic Labs shared model folder under ~/Library/Application Support";
    const PKG_LOOKED_AT: &str = "RIGHTKIT_PARAKEET_COREML_PKG_DIR, or ~/Library/Application Support/ScrapeRight/models/asr/tdt_w15_p6";
    use super::*;
    use rightkit_asr::{ComputeUnits, CoreMlParakeet, CoreMlParakeetOptions, Transcriber};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    fn home(rel: &str) -> String {
        format!("{}/Library/Application Support/{rel}", std::env::var("HOME").unwrap())
    }

    pub fn bundle() -> Option<PathBuf> {
        first_existing(
            "RIGHTKIT_PARAKEET_COREML_DIR",
            &[
                &home("HeardRightNext/models/parakeet-tdt-v3"),
                &home("Orthic Labs/shared/models/parakeet-tdt-v3-coreml-compiled-5f8888117f47"),
            ],
        )
    }

    fn package_bundle() -> Option<PathBuf> {
        first_existing("RIGHTKIT_PARAKEET_COREML_PKG_DIR", &[&home("ScrapeRight/models/asr/tdt_w15_p6")])
    }

    fn load(options: CoreMlParakeetOptions) -> Option<CoreMlParakeet> {
        crate::coreml_cache::prune_stale();
        Some(CoreMlParakeet::load(&bundle()?, options).expect("load CoreML Parakeet"))
    }

    /// One production-default engine reused by every default-options test.
    static SHARED: Mutex<Option<CoreMlParakeet>> = Mutex::new(None);

    fn with_engine<R>(f: impl FnOnce(&mut CoreMlParakeet) -> R) -> Option<R> {
        let mut guard = SHARED.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            *guard = load(CoreMlParakeetOptions::default());
        }
        guard.as_mut().map(f)
    }

    macro_rules! engine {
        ($f:expr) => {
            gate!(with_engine($f), "CoreML Parakeet bundle", COREML_LOOKED_AT)
        };
    }

    #[test]
    fn bundle_geometry_vocab_and_identity_are_read_from_the_real_model() {
        let (meta, pieces, fp, win) = engine!(|e| (
            e.meta().clone(),
            e.pieces().to_vec(),
            e.model_fingerprint().to_string(),
            e.window_samples()
        ));
        assert!(meta.is_tdt);
        assert_eq!((meta.n_mel, meta.mel_frames, meta.blank_id), (128, 1500, 8192));
        assert_eq!(meta.durations, vec![0, 1, 2, 3, 4]);
        assert_eq!(win, 240_000);
        assert_eq!(pieces.len(), 8192, "blank is NOT a vocab row (declared, not inferred)");
        assert!(pieces.iter().any(|p| p == "\u{2581}the"));
        assert_eq!(fp.len(), 16);
        let again = rightkit_asr::parakeet_coreml::sampled_model_fingerprint(&bundle().unwrap()).unwrap();
        assert_eq!(fp, again, "fingerprint must be deterministic");
    }

    #[test]
    fn short_clip_keeps_keywords_with_monotonic_word_timing_and_exports() {
        let (r, stats) = engine!(|e| e.transcribe_long(&wav("short_9s.wav"), None, &|_| {}).unwrap().into_parts());
        eprintln!("short: {} | {stats:?}", r.text);
        assert_keywords(&r.text, SHORT);
        assert!(!r.tokens.is_empty());
        assert!(r.tokens.iter().all(|t| t.start >= 0.0 && t.end >= t.start && t.end <= 9.7 + 0.2), "{:?}", r.tokens);
        assert!(r.tokens.windows(2).all(|p| p[1].start >= p[0].start));
        assert_eq!(stats.windows, 1);
        assert!(stats.emitted_tokens >= SHORT.len());
        let ex = longform::timed_exports(&r.tokens);
        let srt = ex.srt.expect("timed pieces must yield SRT");
        assert_keywords(&srt, SHORT);
        assert!(srt.contains(" --> ") && ex.vtt.unwrap().starts_with("WEBVTT"));
        let words = longform::timed_pieces_to_words(&r.tokens);
        assert!(words.windows(2).all(|p| p[0].end_ms <= p[1].start_ms), "word ends are clamped to the next start");
    }

    #[test]
    fn long_clip_crosses_windows_reports_progress_and_stays_monotonic() {
        let audio = wav("long_38s.wav");
        let events = Mutex::new(Vec::new());
        let t = engine!(|e| e
            .transcribe_long(&audio, None, &|p| events.lock().unwrap().push(*p))
            .unwrap());
        eprintln!("long: {} | windows={} {:?}", t.result.text, t.windows, t.stats);
        assert_keywords(&t.result.text, LONG);
        assert!(t.windows >= 3, "38 s needs >= 3 padded windows, got {}", t.windows);
        assert_eq!(t.stats.windows, t.windows);
        let ev = events.lock().unwrap();
        assert_eq!(ev.len(), t.windows);
        assert!(ev.windows(2).all(|p| p[1].samples_done >= p[0].samples_done));
        assert_eq!(ev.last().unwrap().samples_done, audio.len());
        assert!(t.result.tokens.windows(2).all(|p| p[1].start >= p[0].start), "timeline must be monotonic");
        assert!(t.result.tokens.last().unwrap().end > 30.0, "last window must contribute");
        // No word is welded across a seam: every alphabetic run is a word of the clip's vocabulary scale.
        assert!(!t.result.text.contains("  "));
    }

    #[test]
    fn exact_seam_policy_decodes_the_long_clip_with_monotonic_timing() {
        let mut opts = CoreMlParakeetOptions::default().verbatim();
        opts.seam = rightkit_asr::SeamPolicy::Exact;
        let e = gate!(load(opts), "CoreML Parakeet bundle", COREML_LOOKED_AT);
        let t = e.transcribe_long(&wav("long_38s.wav"), None, &|_| {}).unwrap();
        assert_keywords(&t.result.text, LONG);
        assert!(t.result.tokens.windows(2).all(|p| p[1].start >= p[0].start));
    }

    #[test]
    fn cancel_flag_stops_decoding_before_the_next_window() {
        let audio = wav("long_38s.wav");
        let cancel = AtomicBool::new(false);
        let err = engine!(|e| e
            .transcribe_long(&audio, Some(&cancel), &|p| {
                if p.windows_done == 1 {
                    cancel.store(true, Ordering::SeqCst);
                }
            })
            .unwrap_err());
        assert!(matches!(err, rightkit_asr::Error::Cancelled), "{err}");
    }

    #[test]
    fn single_window_primitive_rejects_audio_longer_than_the_static_window() {
        let audio = wav("long_38s.wav");
        let err = engine!(|e| e.transcribe_window(&audio).unwrap_err());
        assert!(err.to_string().contains("transcribe_long"), "{err}");
    }

    #[test]
    fn greedy_is_deterministic_and_beam_keeps_every_keyword() {
        let audio = wav("short_9s.wav");
        let mut greedy = gate!(
            load(CoreMlParakeetOptions::default().greedy()),
            "CoreML Parakeet bundle",
            COREML_LOOKED_AT
        );
        let a = greedy.transcribe(&audio).unwrap();
        let b = greedy.transcribe(&audio).unwrap();
        assert_eq!(a.text, b.text, "greedy decode must be deterministic");
        assert_keywords(&a.text, SHORT);
        // width 1 + reward 0 is the pre-beam loop: switching the SAME engine to
        // width 2 / production reward may change text but must keep content.
        greedy.set_beam_width(2);
        let c = greedy.transcribe(&audio).unwrap();
        assert_keywords(&c.text, SHORT);
        eprintln!("greedy: {} | beam2(no reward): {}", a.text, c.text);
    }

    #[test]
    fn predictor_cache_never_changes_text_and_saves_decoder_calls() {
        let audio = wav("short_9s.wav");
        let (cached_text, cached) = engine!(|e| {
            let t = e.transcribe_long(&audio, None, &|_| {}).unwrap();
            (t.result.text, t.stats)
        });
        let opts = CoreMlParakeetOptions { predictor_cache: false, ..Default::default() };
        let uncached_engine = gate!(load(opts), "CoreML Parakeet bundle", COREML_LOOKED_AT);
        let t = uncached_engine.transcribe_long(&audio, None, &|_| {}).unwrap();
        assert_eq!(t.result.text, cached_text, "cache is an optimization, not a semantic");
        assert!(cached.predictor_cache_hits > 0, "{cached:?}");
        assert!(
            cached.predictor_calls < t.stats.predictor_calls,
            "cache must save TextDecoder calls: cached {} vs uncached {}",
            cached.predictor_calls,
            t.stats.predictor_calls
        );
        assert_eq!(cached.joint_calls, t.stats.joint_calls, "Joint is frame-dependent and never cached");
    }

    #[test]
    fn time_synchronous_pruning_is_opt_in_and_keeps_content() {
        let opts = CoreMlParakeetOptions {
            pruning: rightkit_asr::parakeet_core::PruningMode::TimeSynchronous,
            ..Default::default()
        };
        let e = gate!(load(opts), "CoreML Parakeet bundle", COREML_LOOKED_AT);
        let r = e.transcribe(&wav("short_9s.wav")).unwrap();
        assert_keywords(&r.text, SHORT);
    }

    #[test]
    fn context_bias_steers_a_homophone_and_clear_restores_default() {
        // "mangoes" in the clip: bias a respelling and require a strong enough
        // bias to flip the decode while a weak one leaves it alone.
        let audio = wav("short_9s.wav");
        let (base, weak, strong, installed, restored) = engine!(|e| {
            e.clear_context_bias();
            let base = e.transcribe(&audio).unwrap().text;
            let installed = e.set_context_bias_phrases(["Mongoes"], 0.5);
            let weak = e.transcribe(&audio).unwrap().text;
            let mut strong = String::new();
            for score in [1.0f32, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0] {
                e.set_context_bias_phrases(["Mongoes"], score);
                let t = e.transcribe(&audio).unwrap().text;
                eprintln!("score {score}: {t}");
                if t != base && strong.is_empty() {
                    strong = t;
                }
            }
            e.clear_context_bias();
            let restored = e.transcribe(&audio).unwrap().text;
            (base, weak, strong, installed, restored)
        });
        eprintln!("base: {base}\nweak: {weak}\nstrong: {strong}");
        assert_eq!(installed, 1);
        assert_eq!(base, weak, "a 0.5 nudge must not rewrite a confident decode");
        assert_ne!(base, strong, "a strong phrase bias must change the decode");
        assert!(strong.to_lowercase().contains("mongoes"), "{strong}");
        assert_eq!(base, restored, "clearing bias restores the exact default decode");
    }

    #[test]
    fn n0_hesitation_policy_applies_to_real_vocab_pieces_and_verbatim_keeps_them() {
        let id = |e: &CoreMlParakeet, p: &str| {
            let cased = format!("\u{2581}{}{}", p[..1].to_uppercase(), &p[1..]);
            let plain = format!("\u{2581}{p}");
            e.pieces()
                .iter()
                .position(|x| *x == plain)
                .or_else(|| e.pieces().iter().position(|x| *x == cased))
                .unwrap_or_else(|| panic!("{p} not in vocab"))
        };
        let (n0, kept) = {
            let (ids, n0) = engine!(|e| {
                let ids = [id(e, "the"), id(e, "uh"), id(e, "and")];
                let hits: Vec<_> = ids.iter().enumerate().map(|(i, &t)| (t, i as f32 * 0.4, 1usize)).collect();
                (ids, e.assemble(&hits))
            });
            let verbatim = gate!(
                load(CoreMlParakeetOptions::default().verbatim()),
                "CoreML Parakeet bundle",
                COREML_LOOKED_AT
            );
            let hits: Vec<_> = ids.iter().enumerate().map(|(i, &t)| (t, i as f32 * 0.4, 1usize)).collect();
            (n0, verbatim.assemble(&hits))
        };
        assert_eq!(n0.text, "the and");
        assert_eq!(n0.tokens.len(), 2);
        assert_eq!(n0.tokens[1].start, 0.8, "surviving piece keeps its own timing");
        assert_eq!(kept.text, "the uh and");
    }

    #[test]
    fn transcriber_trait_returns_word_timestamps_and_downmixes_stereo() {
        let mono = wav("short_9s.wav");
        let stereo: Vec<f32> = mono.iter().flat_map(|&s| [s, s]).collect();
        let (words, from_stereo) = engine!(|e| (
            e.transcribe_samples(mono.clone(), 16_000, 1, Some(TimestampMode::Words)).unwrap(),
            e.transcribe_samples(stereo.clone(), 16_000, 2, None).unwrap(),
        ));
        assert!(words.tokens.len() >= SHORT.len() && words.tokens.len() < 40, "{:?}", words.tokens.len());
        assert!(words.tokens.iter().all(|t| !t.text.contains(' ')), "word mode yields whole words");
        assert_eq!(from_stereo.text, words.text, "L+R duplicate downmixes to the same mono decode");
        let err = engine!(|e| e.transcribe_samples(mono.clone(), 44_100, 1, None).unwrap_err());
        assert!(err.to_string().contains("sample rate"), "{err}");
    }

    #[test]
    fn mlpackage_stages_compile_once_into_the_supplied_cache_and_decode() {
        let dir = gate!(package_bundle(), "ScrapeRight tdt_w15_p6 .mlpackage bundle", PKG_LOOKED_AT);
        let cache = std::env::temp_dir().join(format!("rightkit-asr-pkg-cache-{}", std::process::id()));
        let opts = CoreMlParakeetOptions {
            compile_cache_dir: Some(cache.clone()),
            encoder_compute: ComputeUnits::CpuAndGpu,
            decoder_compute: ComputeUnits::CpuAndGpu,
            ..Default::default()
        };
        crate::coreml_cache::prune_stale();
        let e = CoreMlParakeet::load(&dir, opts).expect("load .mlpackage bundle");
        let compiled: Vec<_> = std::fs::read_dir(&cache).unwrap().filter_map(|x| x.ok()).collect();
        assert_eq!(compiled.len(), 4, "one compiled .mlmodelc per stage in the supplied cache");
        let r = e.transcribe(&wav("short_9s.wav")).unwrap();
        eprintln!("mlpackage: {}", r.text);
        assert_keywords(&r.text, SHORT);
        std::fs::remove_dir_all(&cache).unwrap();
    }

    #[test]
    fn broken_bundle_is_a_load_error_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("rightkit-asr-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pipeline.json"), r#"{"n_mel": 0}"#).unwrap();
        let err = CoreMlParakeet::load(&dir, CoreMlParakeetOptions::default()).err().expect("must fail");
        assert!(err.to_string().contains("pipeline.json"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    trait IntoParts {
        fn into_parts(self) -> (rightkit_asr::parakeet_core::TranscriptionResult, rightkit_asr::parakeet_coreml::DecodeStats);
    }
    impl IntoParts for rightkit_asr::LongFormTranscript {
        fn into_parts(self) -> (rightkit_asr::parakeet_core::TranscriptionResult, rightkit_asr::parakeet_coreml::DecodeStats) {
            (self.result, self.stats)
        }
    }

    /// One loaded engine, identical audio: the per-call policy alone decides
    /// whether `uh`/`um` survive (dictation strips, verbatim keeps), and every
    /// other word keeps the model's own timing in both modes.
    #[test]
    fn per_request_verbatim_keeps_fillers_and_the_model_timings() {
        use rightkit_asr::parakeet_core::DetokPolicy;
        let audio = wav("filler_retake.wav");
        let (clean, raw) = engine!(|e| {
            let mut verbatim = DetokPolicy::canonical_rust();
            verbatim.strip_fillers = false;
            let clean = e.transcribe_long_with(&audio, DetokPolicy::canonical_rust(), None, &|_| {}).unwrap().result;
            let raw = e.transcribe_long_with(&audio, verbatim, None, &|_| {}).unwrap().result;
            (clean, raw)
        });
        eprintln!("clean: {}\nverbatim: {}", clean.text, raw.text);
        let filler = |w: &String| w == "um" || w == "uh";
        let (cw, rw) = (words(&clean.text), words(&raw.text));
        assert!(!cw.iter().any(filler), "dictation policy must strip fillers: {}", clean.text);
        assert!(rw.iter().any(filler), "verbatim must keep the recorded filler: {}", raw.text);
        assert_eq!(rw.iter().filter(|w| !filler(w)).cloned().collect::<Vec<_>>(), cw, "only fillers differ");
        let timed = |r: &rightkit_asr::parakeet_core::TranscriptionResult| {
            longform::timed_pieces_to_words(&r.tokens)
                .into_iter()
                .filter(|w| !matches!(w.text.trim().trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase().as_str(), "um" | "uh"))
                .map(|w| w.start_ms as i64)
                .collect::<Vec<_>>()
        };
        assert_eq!(timed(&raw), timed(&clean), "non-filler word starts are the model's in both modes");
    }
}

// ---------------------------------------------------------------------------
// ONNX (any OS): model not present locally; gate records the exact files.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "needs an ONNX Parakeet TDT export (RIGHTKIT_PARAKEET_ONNX_DIR); run with --ignored"]
fn onnx_transducer_decodes_the_fixtures_when_the_export_is_present() {
    use rightkit_asr::{ExecutionConfig, ExecutionProvider, Transducer, TransducerOptions};
    let dir = gate!(
        first_existing("RIGHTKIT_PARAKEET_ONNX_DIR", &[]),
        "ONNX Parakeet TDT export",
        "RIGHTKIT_PARAKEET_ONNX_DIR (encoder-model.onnx[+.data], decoder_joint-model.onnx, nemo128.onnx, vocab.txt, config.json; plus the ORT dylib named by ORT_DYLIB_PATH)"
    );
    let mut t = Transducer::load_with_options(
        &dir,
        Some(ExecutionConfig::new().with_execution_provider(ExecutionProvider::Cpu)),
        TransducerOptions::default(),
    )
    .expect("load ONNX transducer");
    let text = t.transcribe(&wav("short_9s.wav")).unwrap();
    assert_keywords(&text, SHORT);
    // Long-form through the shared padded-window route, not a raw loop.
    let long = longform::transcribe_padded_window(
        &wav("long_38s.wav"),
        &LongFormOptions::default(),
        None,
        &|_| {},
        |seg| {
            let r = t.transcribe_result(seg)?;
            Ok(rightkit_asr::parakeet_core::TranscriptionResult {
                text: r.text,
                tokens: r
                    .tokens
                    .into_iter()
                    .map(|k| rightkit_asr::parakeet_core::TimedToken { text: k.text, start: k.start, end: k.end })
                    .collect(),
            })
        },
    )
    .unwrap();
    assert_keywords(&long.text, LONG);
}
