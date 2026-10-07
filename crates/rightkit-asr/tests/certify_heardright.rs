//! Phase 6 certification: rightkit-asr must reproduce HeardRight's own engine.
//!
//! Each fixture is decoded twice on the same machine: by the installed HeardRight
//! (through its live speech service, i.e. HeardRight's engine and model) and by
//! rightkit-asr's production-default CoreML path on HeardRight's model bundle.
//! The decoder's words must match exactly, with timings within one model frame
//! (80 ms). The final text is reported but not gated: HeardRight polishes it
//! afterwards (number formatting, line breaks), which is its product text layer.
//!
//! Opt-in: `RIGHTKIT_ASR_CERTIFY=1` (needs a running HeardRight). Writes a JSON
//! report to `RIGHTKIT_ASR_CERTIFY_REPORT` when set.
#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use rightkit_asr::{longform, CoreMlParakeet, CoreMlParakeetOptions};
use rightkit_speech_client as sc;

const FRAME_MS: i64 = 80;

fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    ["short_9s.wav", "long_38s.wav", "filler_retake.wav"]
        .iter()
        .map(|f| dir.join(f))
        .collect()
}

fn bundle() -> PathBuf {
    std::env::var_os("RIGHTKIT_PARAKEET_COREML_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap())
                .join("Library/Application Support/HeardRightNext/models/parakeet-tdt-v3")
        })
}

fn wav(p: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(p).unwrap();
    assert_eq!((r.spec().sample_rate, r.spec().channels), (16_000, 1));
    r.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect()
}

#[test]
fn rightkit_asr_matches_installed_heardright() {
    if std::env::var_os("RIGHTKIT_ASR_CERTIFY").is_none() {
        eprintln!("SKIP: set RIGHTKIT_ASR_CERTIFY=1 with HeardRight running");
        return;
    }
    let root = sc::default_suite_root().expect("suite root");
    let mut hr = sc::connect(&root).expect("installed HeardRight speech service");
    let host = hr.engine_version().to_string();
    let engine = CoreMlParakeet::load(&bundle(), CoreMlParakeetOptions::default()).expect("load bundle");
    let mut report = Vec::new();
    let mut failures = Vec::new();
    for f in fixtures() {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let theirs = hr.transcribe_file(&f, Duration::from_secs(600)).expect("HeardRight transcript");
        // HeardRight conditions file audio (default policy) before decoding.
        let ours = engine
            .transcribe(&rightkit_asr::conditioning::condition_for_asr(&wav(&f), 16_000, "default"))
            .unwrap();
        let ours_words: Vec<(String, i64, i64)> = longform::timed_pieces_to_words(&ours.tokens)
            .into_iter()
            .map(|w| (w.text.trim().to_string(), (w.start * 1000.0).round() as i64, (w.end * 1000.0).round() as i64))
            .collect();
        let theirs_words: Vec<(String, i64, i64)> =
            theirs.words.iter().map(|w| (w.text.trim().to_string(), w.start_ms, w.end_ms)).collect();
        // HeardRight's file text is polished after decoding (digits, line breaks); the word
        // list is the decoder's own output, so words + timings are the decoder parity check.
        let norm = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
        let text_eq = norm(&ours.text) == norm(&theirs.text);
        let same_words = ours_words.len() == theirs_words.len()
            && ours_words.iter().zip(&theirs_words).all(|(a, b)| a.0 == b.0);
        let max_dt = ours_words
            .iter()
            .zip(&theirs_words)
            .map(|(a, b)| (a.1 - b.1).abs().max((a.2 - b.2).abs()))
            .max()
            .unwrap_or(0);
        eprintln!("{name}: text_eq={text_eq} words={}/{} same_words={same_words} max_dt={max_dt}ms", ours_words.len(), theirs_words.len());
        for (i, (a, b)) in ours_words.iter().zip(&theirs_words).enumerate() {
            if a.0 != b.0 || (a.1 - b.1).abs() > FRAME_MS {
                eprintln!("  word {i}: heardright {:?}  rightkit {:?}", b, a);
            }
        }
        if !(same_words && max_dt <= FRAME_MS) {
            failures.push(name.clone());
        }
        report.push(serde_json::json!({
            "fixture": name, "text_equal": text_eq, "words_equal": same_words,
            "words": ours_words.len(), "max_timing_delta_ms": max_dt,
            "heardright_text": theirs.text, "rightkit_text": ours.text,
        }));
    }
    if let Some(out) = std::env::var_os("RIGHTKIT_ASR_CERTIFY_REPORT") {
        let doc = serde_json::json!({
            "gate": "rightkit-asr phase 6 parity", "platform": "macos-coreml",
            "heardright_host": host, "model_bundle": bundle(), "model_fingerprint": engine.model_fingerprint(),
            "tolerance_ms": FRAME_MS, "results": report,
        });
        std::fs::write(out, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
    }
    assert!(failures.is_empty(), "parity failures: {failures:?}");
}
