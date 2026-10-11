//! Real whisper.cpp 1.9.3 checks for the 0.2.0 route: language identification,
//! the language table against the library, word timing shape, text-only decode
//! and the auto-detect option. Uses the crate's existing fixture audio.
//!
//! Gates (a missing asset prints a SKIP line and the test passes; set
//! `RIGHTKIT_REQUIRE_ASSETS=1` to make a missing asset fail):
//! - `RIGHTKIT_WHISPER_LIB`: libwhisper built from v1.9.3 (`libwhisper.dylib`,
//!   `whisper.dll`, `libwhisper.so`).
//! - `RIGHTKIT_WHISPER_GGML`: a multilingual ggml model (detection needs one;
//!   an English-only model fails the detection test by design).

#[macro_use]
mod common;

use libloading::Library;
use rightkit_asr::{
    AudioSource, CancelToken, DecodeMode, DecodeQuality, Error, InterruptReason, Language,
    ProgressCallback, Whisper, WhisperModel, WhisperOptions, WHISPER_LANGUAGES,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::path::{Path, PathBuf};

fn env_file(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from).filter(|p| p.exists())
}

fn wav(name: &str) -> Vec<f32> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    let mut r = hound::WavReader::open(p).unwrap();
    assert_eq!((r.spec().sample_rate, r.spec().channels), (16_000, 1));
    r.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect()
}

fn load(options: WhisperOptions) -> Option<Whisper> {
    let library = env_file("RIGHTKIT_WHISPER_LIB")?;
    let model = env_file("RIGHTKIT_WHISPER_GGML")?;
    Some(
        Whisper::load(WhisperModel::Ggml { library, model }, options).expect("load whisper.cpp"),
    )
}

/// Words of the short fixture the transcript must contain (same list as e2e_whisper).
const SHORT_KEYWORDS: &[&str] = &["timer", "minutes", "remind"];

#[test]
fn language_table_matches_the_pinned_library() {
    let library = gate!(env_file("RIGHTKIT_WHISPER_LIB"), "libwhisper 1.9.3", "RIGHTKIT_WHISPER_LIB");
    // SAFETY: loading the pinned library runs its initialisers, as the crate does at load time.
    let lib = unsafe { Library::new(&library) }.expect("open whisper library");
    type FnMaxId = unsafe extern "C" fn() -> std::ffi::c_int;
    type FnStr = unsafe extern "C" fn(std::ffi::c_int) -> *const std::ffi::c_char;
    // SAFETY: symbol types as declared in whisper.h v1.9.3 (whisper.h:358, :367).
    let (max_id, lang_str) = unsafe {
        let max: libloading::Symbol<FnMaxId> = lib.get(b"whisper_lang_max_id\0").expect("max id");
        let s: libloading::Symbol<FnStr> = lib.get(b"whisper_lang_str\0").expect("lang str");
        ((*max)(), *s)
    };
    assert_eq!(max_id as usize + 1, WHISPER_LANGUAGES.len());
    for id in 0..=max_id {
        // SAFETY: id within 0..=max_id; returns a static NUL-terminated string.
        let code = unsafe { std::ffi::CStr::from_ptr(lang_str(id)) }.to_str().unwrap();
        assert_eq!(WHISPER_LANGUAGES[id as usize], code, "table entry {id}");
    }
}

#[test]
fn detect_language_identifies_english_fixture() {
    let w = gate!(load(WhisperOptions::default()), "whisper.cpp library and ggml model", "RIGHTKIT_WHISPER_LIB and RIGHTKIT_WHISPER_GGML");
    let det = w.detect_language(&wav("short_9s.wav")).expect("detect language");
    assert_eq!(det.lang, "en");
    assert!(det.probability > 0.5, "probability {}", det.probability);
    assert_eq!(det.candidates.len(), WHISPER_LANGUAGES.len());
    assert!(det.candidates.windows(2).all(|p| p[0].1 >= p[1].1), "candidates sorted descending");
    let sum: f32 = det.candidates.iter().map(|c| c.1).sum();
    assert!((sum - 1.0).abs() < 0.01, "probabilities sum to {sum}");
    assert_eq!(det.candidates[0].0, det.lang);
}

#[test]
fn word_timings_have_the_documented_shape() {
    let options = WhisperOptions { language: Language::Code("en".into()), token_timestamps: true, ..Default::default() };
    let mut w = gate!(load(options), "whisper.cpp library and ggml model", "RIGHTKIT_WHISPER_LIB and RIGHTKIT_WHISPER_GGML");
    let t = w.transcribe_detailed(AudioSource::Pcm16k(wav("short_9s.wav"))).expect("decode");
    assert!(!t.segments.is_empty());
    assert!(!t.words.is_empty(), "token timestamps must yield words");
    assert!(t.detection.is_none(), "fixed language does not detect");
    assert_eq!(t.language, "en");
    let mut prev_start = 0u64;
    for w in &t.words {
        assert_eq!(w.sample_rate, 16_000);
        assert!(!w.word.text.trim().is_empty());
        assert!(!w.word.text.contains("[_"), "special tokens must be excluded: {}", w.word.text);
        assert!(w.word.start_ms <= w.word.end_ms, "{:?}", w.word);
        assert_eq!(w.word.start_ms, w.start_sample * 1000 / 16_000, "ms derives from samples");
        assert_eq!(w.word.end_ms, w.end_sample * 1000 / 16_000);
        assert!((0.0..=1.0).contains(&w.probability), "probability {}", w.probability);
        assert!(w.start_sample >= prev_start, "word starts must not move backwards");
        prev_start = w.start_sample;
    }
    let joined: String = t.words.iter().map(|w| w.word.text.to_lowercase()).collect::<Vec<_>>().join(" ");
    for k in SHORT_KEYWORDS {
        assert!(joined.contains(k), "missing {k} in {joined}");
    }
}

#[test]
fn text_only_mode_has_text_and_no_word_timings() {
    let options = WhisperOptions { language: Language::Code("en".into()), mode: DecodeMode::TextOnly, ..Default::default() };
    let mut w = gate!(load(options), "whisper.cpp library and ggml model", "RIGHTKIT_WHISPER_LIB and RIGHTKIT_WHISPER_GGML");
    let t = w.transcribe_detailed(AudioSource::Pcm16k(wav("short_9s.wav"))).expect("decode");
    assert!(t.words.is_empty());
    let lower = t.text.to_lowercase();
    for k in SHORT_KEYWORDS {
        assert!(lower.contains(k), "missing {k} in {lower}");
    }
}

#[test]
fn text_only_with_token_timestamps_is_rejected() {
    let options = WhisperOptions { mode: DecodeMode::TextOnly, token_timestamps: true, ..Default::default() };
    let mut w = gate!(load(options), "whisper.cpp library and ggml model", "RIGHTKIT_WHISPER_LIB and RIGHTKIT_WHISPER_GGML");
    let err = w.transcribe_detailed(AudioSource::Pcm16k(wav("short_9s.wav"))).unwrap_err();
    assert!(matches!(err, rightkit_asr::Error::Config(_)), "{err:?}");
}

#[test]
fn auto_detect_option_locks_and_reports_the_language() {
    let options = WhisperOptions { detect_language: true, ..Default::default() };
    let mut w = gate!(load(options), "whisper.cpp library and ggml model", "RIGHTKIT_WHISPER_LIB and RIGHTKIT_WHISPER_GGML");
    let t = w.transcribe_detailed(AudioSource::Pcm16k(wav("short_9s.wav"))).expect("decode");
    let det = t.detection.expect("detection reported");
    assert_eq!(det.lang, "en");
    assert_eq!(t.language, det.lang, "the decode uses the detected language");
}

#[test]
fn detect_option_with_fixed_language_is_rejected() {
    let options = WhisperOptions {
        language: Language::Code("en".into()),
        detect_language: true,
        ..Default::default()
    };
    let mut w = gate!(load(options), "whisper.cpp library and ggml model", "RIGHTKIT_WHISPER_LIB and RIGHTKIT_WHISPER_GGML");
    let err = w.transcribe_detailed(AudioSource::Pcm16k(wav("short_9s.wav"))).unwrap_err();
    assert!(matches!(err, rightkit_asr::Error::Config(_)), "{err:?}");
}

#[test]
fn pre_tripped_cancel_returns_interrupted_without_decoding() {
    let token = CancelToken::new();
    token.cancel();
    let options = WhisperOptions { cancel: Some(token), ..Default::default() };
    let mut w = gate!(load(options), "whisper.cpp library and ggml model", "RIGHTKIT_WHISPER_LIB and RIGHTKIT_WHISPER_GGML");
    let err = w.transcribe_detailed(AudioSource::Pcm16k(wav("short_9s.wav"))).unwrap_err();
    assert!(err.is_cancelled(), "{err:?}");
    let i = err.interrupted().expect("typed interruption");
    assert_eq!(i.reason, InterruptReason::Cancelled);
    assert_eq!(i.windows_completed, 0);
    assert!(i.partial.segments.is_empty());
}

#[test]
fn zero_total_deadline_stops_with_a_deadline_interruption() {
    let options = WhisperOptions { total_deadline: Some(Duration::ZERO), ..Default::default() };
    let mut w = gate!(load(options), "whisper.cpp library and ggml model", "RIGHTKIT_WHISPER_LIB and RIGHTKIT_WHISPER_GGML");
    let err = w.transcribe_detailed(AudioSource::Pcm16k(wav("short_9s.wav"))).unwrap_err();
    assert_eq!(err.interrupted().expect("typed interruption").reason, InterruptReason::DeadlineExceeded);
}

#[test]
fn cancel_during_decode_stops_the_window_and_progress_is_ordered() {
    let token = CancelToken::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let trip = token.clone();
    let options = WhisperOptions {
        cancel: Some(token),
        // Trip from inside whisper's own progress callback: the window is in flight.
        progress: Some(ProgressCallback::new(move |p| {
            sink.lock().unwrap().push(p.fraction);
            trip.cancel();
        })),
        ..Default::default()
    };
    let mut w = gate!(load(options), "whisper.cpp library and ggml model", "RIGHTKIT_WHISPER_LIB and RIGHTKIT_WHISPER_GGML");
    let err = w.transcribe_detailed(AudioSource::Pcm16k(wav("short_9s.wav"))).unwrap_err();
    assert!(err.is_cancelled(), "{err:?}");
    let seen = seen.lock().unwrap();
    assert!(seen.windows(2).all(|p| p[1] >= p[0]), "progress never decreases: {seen:?}");
}

#[test]
fn progress_completes_and_cli_quality_decodes() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let options = WhisperOptions {
        language: Language::Code("en".into()),
        quality: DecodeQuality::whisper_cli(),
        progress: Some(ProgressCallback::new(move |p| sink.lock().unwrap().push(*p))),
        ..Default::default()
    };
    let mut w = gate!(load(options), "whisper.cpp library and ggml model", "RIGHTKIT_WHISPER_LIB and RIGHTKIT_WHISPER_GGML");
    let t = w.transcribe_detailed(AudioSource::Pcm16k(wav("short_9s.wav"))).expect("decode");
    let lower = t.text.to_lowercase();
    assert!(SHORT_KEYWORDS.iter().any(|k| lower.contains(k)), "transcript: {}", t.text);
    assert!(t.segments.iter().all(|s| s.text == s.raw_text.trim()));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.last().expect("progress reported").fraction, 1.0);
}

#[test]
fn invalid_quality_is_a_config_error() {
    let options = WhisperOptions {
        quality: DecodeQuality { beam_size: 0, ..DecodeQuality::default() },
        ..Default::default()
    };
    let mut w = gate!(load(options), "whisper.cpp library and ggml model", "RIGHTKIT_WHISPER_LIB and RIGHTKIT_WHISPER_GGML");
    let err = w.transcribe_detailed(AudioSource::Pcm16k(wav("short_9s.wav"))).unwrap_err();
    assert!(matches!(err, Error::Config(_)), "{err:?}");
}
