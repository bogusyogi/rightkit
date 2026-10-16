//! End-to-end Whisper decoding with real audio and real models.
//!
//! Gates (a missing asset prints a SKIP line and the test passes; set
//! `RIGHTKIT_REQUIRE_ASSETS=1` to make a missing asset fail):
//! - CoreML (macOS): WhisperKit `.mlmodelc` dir (~600 MB; AudioEncoder,
//!   TextDecoder, tokenizer.json). `RIGHTKIT_WHISPER_COREML_DIR` or the local
//!   HeardRight/ScrapeRight registry.
//! - whisper.cpp: `RIGHTKIT_WHISPER_LIB` (libwhisper built from v1.9.3:
//!   `cmake -DBUILD_SHARED_LIBS=ON -DGGML_METAL=ON`, `whisper.dll` on Windows)
//!   plus `RIGHTKIT_WHISPER_GGML` (ggml model, e.g. turbo q5_k ~574 MB).
//! - VAD: `RIGHTKIT_VAD_MODEL` (Silero op15 ONNX, 2 MB).
#[macro_use]
mod common;

#[cfg(target_os = "macos")]
mod coreml_cache;

use rightkit_asr::{
    Language, Transcriber, Whisper, WhisperModel, WhisperOptions,
};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::sync::OnceLock;
#[cfg(target_os = "macos")]
use rightkit_asr::transcribe_spans;

/// The approved ONNX Runtime 1.24.4 dylib: `RIGHTKIT_ORT_TEST_DYLIB` when set (a missing file
/// then fails loudly), else the genright download cache found by walking up from this crate.
#[cfg(target_os = "macos")]
fn ort_runtime_file() -> Option<PathBuf> {
    std::env::var_os("RIGHTKIT_ORT_TEST_DYLIB")
        .map(PathBuf::from)
        .or_else(|| common::workspace_asset("genright/.cache/devin-completion-20261006/batch5/voice/dl/onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.1.24.4.dylib"))
}

#[cfg(target_os = "macos")]
fn init_ort_runtime() {
    static READY: OnceLock<()> = OnceLock::new();
    READY.get_or_init(|| {
        use rightkit_ffmpeg::native::{NativeCatalog, NativeLibrary};
        use rightkit_onnx::{
            init_environment, resolve_runtime, CandidateSource, EnvironmentOptions, GlobalPool,
            RuntimeCandidate, runtime_filename,
        };

        // The VAD test checks `ort_runtime_file()` before it gets here.
        let runtime = ort_runtime_file().expect("the VAD test checks ort_runtime_file() first");
        assert!(runtime.is_file(), "no approved ONNX Runtime 1.24.4 dylib at {}; set RIGHTKIT_ORT_TEST_DYLIB", runtime.display());
        let staged_dir = std::env::temp_dir().join(format!("rightkit-asr-ort-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staged_dir);
        std::fs::create_dir_all(&staged_dir).unwrap();
        let staged = staged_dir.join(runtime_filename());
        std::fs::copy(&runtime, &staged).unwrap();
        let catalog = NativeCatalog::embedded().unwrap();
        let record = catalog
            .approved(NativeLibrary::Onnxruntime, "macos", "aarch64")
            .unwrap();
        assert_eq!(record.version, "1.24.4");
        record.verify_file(&staged).expect("ORT dylib must match approved catalog");
        let selection = resolve_runtime([RuntimeCandidate::new(&staged, CandidateSource::Explicit)])
            .expect("approved ORT dylib must resolve before initialization");
        init_environment(
            &selection,
            &EnvironmentOptions { global_pool: Some(GlobalPool::from_budget()) },
        )
        .expect("approved ORT dylib must initialize before any session");
    });
}

const SHORT: (&str, &[&str]) = (
    "short_9s.wav",
    &["timer", "minutes", "remind", "oven", "mangoes", "bananas", "sparkling", "water"],
);
const LONG: (&str, &[&str]) = (
    "long_38s.wav",
    &["healthy", "small", "epoch", "medium", "alive", "rollback", "threshold"] // "watches" omitted: Whisper hears "watchers",
);

fn wav(name: &str) -> Vec<f32> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    let mut r = hound::WavReader::open(p).unwrap();
    assert_eq!((r.spec().sample_rate, r.spec().channels), (16_000, 1));
    r.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect()
}

fn first_existing(env: &str, defaults: &[&str]) -> Option<PathBuf> {
    std::env::var_os(env)
        .map(PathBuf::from)
        .into_iter()
        .chain(defaults.iter().map(PathBuf::from))
        .find(|p| p.exists())
}

// Only the macOS Core ML tests use these.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const COREML_LOOKED_AT: &str = "RIGHTKIT_WHISPER_COREML_DIR or /Volumes/D/claude/heardright/model_registry/coreml/whisper-multi";

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn coreml_dir() -> Option<PathBuf> {
    first_existing(
        "RIGHTKIT_WHISPER_COREML_DIR",
        &["/Volumes/D/claude/heardright/model_registry/coreml/whisper-multi"],
    )
}

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

#[cfg(target_os = "macos")]
mod coreml {
    use super::*;

    fn load(language: Language) -> Option<Whisper> {
        crate::coreml_cache::prune_stale();
        let dir = coreml_dir()?;
        Some(
            Whisper::load(
                WhisperModel::CoreMlDir { dir },
                // CpuAndGpu: the default (CPU+ANE) triggers a cold ANECompilerService
                // compile of the whisper stages that can run for 10+ minutes on a loaded
                // machine, hanging load(). These tests verify decoding/timing, not ANE.
                WhisperOptions { language, compute_units: rightkit_asr::ComputeUnits::CpuAndGpu, ..Default::default() },
            )
            .expect("load CoreML whisper"),
        )
    }

    #[test]
    fn short_clip_auto_language_detects_english_and_keeps_all_keywords() {
        let mut w = gate!(load(Language::Auto), "CoreML whisper dir", COREML_LOOKED_AT);
        let r = w.transcribe_samples(wav(SHORT.0), 16_000, 1, None).unwrap();
        assert_keywords(&r.text, SHORT.1);
        assert_eq!(w.detected_language(), Some("en"));
        assert!(!r.tokens.is_empty());
        assert!(r.tokens.iter().all(|t| t.start >= 0.0 && t.end >= t.start && t.end <= 9.9), "{:?}", r.tokens);
    }

    #[test]
    fn long_clip_crosses_the_30s_window_without_losing_words() {
        let mut w = gate!(load(Language::Code("en".into())), "CoreML whisper dir", COREML_LOOKED_AT);
        let r = w.transcribe_samples(wav(LONG.0), 16_000, 1, None).unwrap();
        assert_keywords(&r.text, LONG.1);
        assert!(r.tokens.windows(2).all(|p| p[1].start >= p[0].start), "timeline must be monotonic");
        assert!(r.tokens.last().unwrap().end > 30.0, "second window must contribute: {:?}", r.tokens.last());
    }

    #[test]
    fn unknown_language_is_rejected_not_silently_english() {
        let mut w = gate!(load(Language::Code("zz".into())), "CoreML whisper dir", COREML_LOOKED_AT);
        assert!(w.transcribe_samples(wav(SHORT.0), 16_000, 1, None).is_err());
    }

    #[test]
    fn vad_chunked_decode_retimes_onto_the_source_timeline() {
        let vad_model = gate!(
            first_existing(
                "RIGHTKIT_VAD_MODEL",
                &["/Volumes/D/claude/heardright/tauri-app-next/src-tauri/resources/vad/silero_vad_16k_op15.onnx"],
            ),
            "Silero VAD model",
            "RIGHTKIT_VAD_MODEL or /Volumes/D/claude/heardright/tauri-app-next/src-tauri/resources/vad/silero_vad_16k_op15.onnx",
        );
        let mut w = gate!(load(Language::Auto), "CoreML whisper dir", COREML_LOOKED_AT);
        let mut audio = vec![0.0f32; 16_000 * 3];
        audio.extend(wav(SHORT.0));
        audio.extend(vec![0.0f32; 16_000 * 3]);
        let _runtime = gate!(ort_runtime_file(), "approved ONNX Runtime 1.24.4 macOS dylib", "RIGHTKIT_ORT_TEST_DYLIB or genright/.cache/devin-completion-20261006/batch5/voice/dl/onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.1.24.4.dylib");
        init_ort_runtime();
        let mut silero = rightkit_vad::Silero::load(vad_model).unwrap();
        let spans = rightkit_vad::detect(&mut silero, &audio, &rightkit_vad::VadConfig::default()).unwrap();
        assert!(!spans.is_empty());
        let chunks = transcribe_spans(&mut w, &audio, &spans, None).unwrap();
        let text = chunks.iter().map(|c| c.result.text.as_str()).collect::<Vec<_>>().join(" ");
        assert_keywords(&text, SHORT.1);
        for c in &chunks {
            for t in &c.result.tokens {
                assert!(t.start >= c.span.start_ms as f32 / 1000.0 - 0.05, "token before its span");
                assert!(t.start >= 2.9 && t.end <= 12.8, "token outside the speech region: {t:?}");
            }
        }
    }
}

const CPP_LOOKED_AT: &str = "RIGHTKIT_WHISPER_LIB and RIGHTKIT_WHISPER_GGML, or the /Volumes/D/claude/genright and /Volumes/D/claude/scraperight defaults";

mod cpp {
    use super::*;

    fn load(language: Language) -> Option<Whisper> {
        let library = first_existing(
            "RIGHTKIT_WHISPER_LIB",
            &["/Volumes/D/claude/genright/.cache/whisper-cpp-1.9.3-metal/libwhisper.dylib"],
        )?;
        let model = first_existing(
            "RIGHTKIT_WHISPER_GGML",
            &["/Volumes/D/claude/scraperight/models/asr/whisper_turbo_q5_k.bin"],
        )?;
        Some(
            Whisper::load(
                WhisperModel::Ggml { library, model },
                WhisperOptions { language, ..Default::default() },
            )
            .expect("load whisper.cpp"),
        )
    }

    #[test]
    fn short_clip_whispercpp_keeps_all_keywords_with_segment_timing() {
        let mut w = gate!(load(Language::Auto), "whisper.cpp library and ggml model", CPP_LOOKED_AT);
        let r = w.transcribe_samples(wav(SHORT.0), 16_000, 1, None).unwrap();
        assert_keywords(&r.text, SHORT.1);
        assert_eq!(w.detected_language(), Some("en"));
        assert!(r.tokens.iter().all(|t| t.end >= t.start && t.end <= 10.2), "{:?}", r.tokens);
    }

    #[test]
    fn long_clip_whispercpp_crosses_the_window() {
        let mut w = gate!(load(Language::Code("en".into())), "whisper.cpp library and ggml model", CPP_LOOKED_AT);
        let r = w.transcribe_samples(wav(LONG.0), 16_000, 1, None).unwrap();
        assert_keywords(&r.text, LONG.1);
        assert!(r.tokens.last().unwrap().end > 30.0);
    }

    #[test]
    fn wrong_library_or_model_is_an_error() {
        assert!(Whisper::load(
            WhisperModel::Ggml { library: "/nonexistent/libwhisper.dylib".into(), model: "/nonexistent.bin".into() },
            WhisperOptions::default()
        )
        .is_err());
    }
}
