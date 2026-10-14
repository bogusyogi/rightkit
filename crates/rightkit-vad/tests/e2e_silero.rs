//! End-to-end: real Silero ONNX model + real speech audio.
//!
//! Gate: needs `silero_vad_16k_op15.onnx` (2 MB, MIT). Set `RIGHTKIT_VAD_MODEL`
//! to its path. When the variable is unset each test prints a SKIP line and
//! returns; when it names a missing file the test panics. The macOS tests also
//! need an ONNX Runtime 1.24.4 dylib, named by `RIGHTKIT_ORT_TEST_DYLIB` or found
//! in the genright download cache. A missing dylib is a SKIP too. Under
//! `RIGHTKIT_REQUIRE_ASSETS=1` every missing asset panics instead.
mod common;

use rightkit_vad::*;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::sync::OnceLock;

/// The approved ONNX Runtime 1.24.4 dylib on macOS: `RIGHTKIT_ORT_TEST_DYLIB` when set (even
/// when it names a missing file, which then fails loudly), else the genright download cache. The
/// cache's place relative to this crate differs between the main checkout and the build mirrors,
/// so search the ancestors for `genright/...` and `claude/genright/...`.
#[cfg(target_os = "macos")]
fn ort_runtime_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("RIGHTKIT_ORT_TEST_DYLIB") {
        return Some(PathBuf::from(explicit));
    }
    let cached = "genright/.cache/devin-completion-20261006/batch5/voice/dl/onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.1.24.4.dylib";
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .flat_map(|a| [a.join(cached), a.join("claude").join(cached)])
        .find(|p| p.is_file())
}

/// True when the ONNX Runtime dylib is absent on macOS. Other hosts are not checked here.
#[cfg(target_os = "macos")]
fn ort_runtime_missing() -> bool {
    ort_runtime_path().is_none()
}

/// Off macOS there is no approved runtime to resolve, so the tests run only against the file
/// an absolute `ORT_DYLIB_PATH` names. Without it the system loader would pick whatever
/// `onnxruntime` the OS ships (Windows has an old one in System32), which is not a test input.
#[cfg(not(target_os = "macos"))]
fn ort_runtime_missing() -> bool {
    !std::env::var_os("ORT_DYLIB_PATH")
        .map(PathBuf::from)
        .is_some_and(|path| path.is_absolute() && path.is_file())
}

fn init_ort_runtime() {
    #[cfg(target_os = "macos")]
    {
        static READY: OnceLock<()> = OnceLock::new();
        READY.get_or_init(|| {
            use rightkit_ffmpeg::native::{NativeCatalog, NativeLibrary};
            use rightkit_onnx::{
                init_environment, resolve_runtime, runtime_filename, CandidateSource,
                EnvironmentOptions, GlobalPool, RuntimeCandidate,
            };

            // Every test calls `need_ort!()` first, so the dylib is present here.
            let runtime =
                ort_runtime_path().expect("tests check the ONNX Runtime before loading it");
            assert!(
                runtime.is_file(),
                "no approved ONNX Runtime 1.24.4 dylib at {}; set RIGHTKIT_ORT_TEST_DYLIB",
                runtime.display()
            );
            let staged_dir =
                std::env::temp_dir().join(format!("rightkit-vad-ort-e2e-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&staged_dir);
            std::fs::create_dir_all(&staged_dir).unwrap();
            let staged = staged_dir.join(runtime_filename());
            std::fs::copy(&runtime, &staged).unwrap();
            let catalog = NativeCatalog::embedded().unwrap();
            let record = catalog
                .approved(NativeLibrary::Onnxruntime, "macos", "aarch64")
                .unwrap();
            assert_eq!(record.version, "1.24.4");
            record
                .verify_file(&staged)
                .expect("ORT dylib must match approved catalog");
            let selection =
                resolve_runtime([RuntimeCandidate::new(&staged, CandidateSource::Explicit)])
                    .expect("approved ORT dylib must resolve before initialization");
            init_environment(
                &selection,
                &EnvironmentOptions {
                    global_pool: Some(GlobalPool::from_budget()),
                },
            )
            .expect("approved ORT dylib must initialize before any session");
        });
    }
}

fn load_model(path: &Path) -> Silero {
    init_ort_runtime();
    Silero::load(path).unwrap()
}

fn model() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("RIGHTKIT_VAD_MODEL")?);
    assert!(
        path.is_file(),
        "RIGHTKIT_VAD_MODEL does not name a file: {}",
        path.display()
    );
    Some(path)
}

fn load_wav() -> Vec<f32> {
    let p = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/short_9s.wav");
    let mut r = hound::WavReader::open(p).unwrap();
    assert_eq!((r.spec().sample_rate, r.spec().channels), (16_000, 1));
    r.samples::<i16>()
        .map(|s| s.unwrap() as f32 / 32768.0)
        .collect()
}

/// Skips the test (or panics under `RIGHTKIT_REQUIRE_ASSETS=1`) when the ONNX Runtime is absent.
macro_rules! need_ort {
    () => {
        if ort_runtime_missing() {
            common::skip_missing(
                "approved ONNX Runtime 1.24.4 (macOS dylib, or the file ORT_DYLIB_PATH names elsewhere)",
                "RIGHTKIT_ORT_TEST_DYLIB or genright/.cache/devin-completion-20261006/batch5/voice/dl/onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.1.24.4.dylib",
            );
            return;
        }
    };
}

/// The Silero model path. Skips the test when the model or the ONNX Runtime is absent.
macro_rules! need_model {
    () => {{
        need_ort!();
        match model() {
            Some(m) => m,
            None => {
                common::skip_missing(
                    "Silero VAD model silero_vad_16k_op15.onnx",
                    "RIGHTKIT_VAD_MODEL",
                );
                return;
            }
        }
    }};
}

fn with_silence(speech: &[f32], lead_ms: usize, tail_ms: usize) -> Vec<f32> {
    let mut v = vec![0.0; lead_ms * 16];
    v.extend_from_slice(speech);
    v.extend(vec![0.0; tail_ms * 16]);
    v
}

#[test]
fn detects_speech_inside_silence_and_not_the_silence() {
    let model = need_model!();
    let audio = with_silence(&load_wav(), 2000, 2000);
    let mut s = load_model(&model);
    let spans = detect(&mut s, &audio, &VadConfig::default()).unwrap();
    assert!(!spans.is_empty());
    let first = spans.first().unwrap();
    let last = spans.last().unwrap();
    assert!(
        first.start_ms >= 1800,
        "leading silence marked speech: {first:?}"
    );
    assert!(
        last.end_ms <= 2000 + 9760 + 200,
        "trailing silence marked speech: {last:?}"
    );
    let covered: u64 = spans.iter().map(Span::len_ms).sum();
    assert!(covered > 6000, "speech under-covered: {covered} ms");
}

#[test]
fn pure_silence_and_noise_floor_yield_no_spans() {
    let model = need_model!();
    let mut s = load_model(&model);
    assert!(
        detect(&mut s, &vec![0.0; 16_000 * 3], &VadConfig::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn streaming_with_odd_callbacks_matches_batch_scores() {
    let model = need_model!();
    let audio = load_wav();
    // Batch reference.
    let mut batch = load_model(&model);
    let mut ref_scores = Vec::new();
    for c in audio.as_chunks::<FRAME_SAMPLES>().0 {
        ref_scores.push(batch.score(c).unwrap());
    }
    // Streaming through awkward callback sizes (HeardRight chunking A/B pattern).
    let mut stream = FrameStream::new(load_model(&model), 0.5);
    let mut got = Vec::new();
    let (mut off, mut i) = (0, 0);
    let pattern = [3840usize, 4160, 137];
    while off < audio.len() {
        let n = pattern[i % 3].min(audio.len() - off);
        got.extend(stream.push(&audio[off..off + n]).unwrap().scores);
        off += n;
        i += 1;
    }
    assert_eq!(got.len(), ref_scores.len());
    for (a, b) in got.iter().zip(&ref_scores) {
        assert!((a - b).abs() < 1e-6, "stream/batch divergence {a} vs {b}");
    }
}

#[test]
fn chunking_respects_max_and_splits_on_quiet_frames() {
    let model = need_model!();
    let one = load_wav();
    // 4 repetitions with 150 ms gaps: one long continuous-ish region (~40 s).
    let mut audio = Vec::new();
    for _ in 0..4 {
        audio.extend_from_slice(&one);
        audio.extend(vec![0.0; 150 * 16]);
    }
    let mut s = load_model(&model);
    let mut scores = Vec::new();
    for c in audio.chunks(FRAME_SAMPLES) {
        let mut f = [0f32; FRAME_SAMPLES];
        f[..c.len()].copy_from_slice(c);
        scores.push(s.score(&f).unwrap());
    }
    let dur = audio.len() as u64 * 1000 / 16_000;
    let spans = scores_to_spans(&scores, &VadConfig::default(), dur);
    let cfg = ChunkConfig {
        merge_gap_ms: 400,
        max_chunk_ms: 12_000,
        min_chunk_ms: 200,
    };
    let chunks = chunk_spans(&spans, &cfg, Some(&scores));
    assert!(chunks.len() >= 3);
    assert!(chunks.iter().all(|c| c.len_ms() <= 12_000 + FRAME_MS));
    // No speech lost: coverage of raw spans by chunks.
    let raw: u64 = spans.iter().map(Span::len_ms).sum();
    let cov: u64 = chunks.iter().map(Span::len_ms).sum();
    assert!(cov >= raw);
}

#[test]
fn missing_model_is_an_error_not_silence() {
    assert!(Silero::load("/nonexistent/silero.onnx").is_err());
}

/// Regression for the CPU path: embedded bytes and a file path load the same
/// model through the same session setup, so scores and spans must agree.
#[test]
fn embedded_bytes_match_path_load_on_cpu() {
    let model = need_model!();
    init_ort_runtime();
    let bytes = std::fs::read(&model).unwrap();
    let audio = load_wav();
    let mut by_path = Silero::load(&model).unwrap();
    let mut by_bytes = Silero::from_bytes(&bytes, SileroOptions::default()).unwrap();
    assert_eq!(by_path.backend(), Backend::SileroCpu);
    assert_eq!(by_bytes.backend(), Backend::SileroCpu);
    let mut max_diff = 0f32;
    for c in audio.as_chunks::<FRAME_SAMPLES>().0 {
        let a = by_path.score(c).unwrap();
        let b = by_bytes.score(c).unwrap();
        max_diff = max_diff.max((a - b).abs());
    }
    assert!(max_diff < 1e-6, "path/bytes divergence {max_diff}");
    let spans_path = detect(&mut by_path, &audio, &VadConfig::default()).unwrap();
    let spans_bytes = detect(&mut by_bytes, &audio, &VadConfig::default()).unwrap();
    assert_eq!(spans_path, spans_bytes);
}

#[test]
fn speech_scorer_reports_cpu_backend_and_matches_detect() {
    let model = need_model!();
    init_ort_runtime();
    let bytes = std::fs::read(&model).unwrap();
    let audio = with_silence(&load_wav(), 1000, 1000);
    let (mut scorer, report) = SpeechScorer::from_bytes(&bytes, SileroOptions::default()).unwrap();
    assert_eq!(
        report,
        ScorerBackend {
            backend: Backend::SileroCpu,
            fallback_reason: None,
        }
    );
    let mut silero = Silero::from_bytes(&bytes, SileroOptions::default()).unwrap();
    let via_scorer = scorer.detect(&audio, &VadConfig::default()).unwrap();
    let via_silero = detect(&mut silero, &audio, &VadConfig::default()).unwrap();
    assert_eq!(via_scorer, via_silero);
    assert!(!via_scorer.is_empty());
}

#[cfg(target_vendor = "apple")]
#[test]
fn coreml_scorer_loads_and_tracks_cpu_scores() {
    let model = need_model!();
    init_ort_runtime();
    let bytes = std::fs::read(&model).unwrap();
    let audio = load_wav();
    let (mut cpu, cpu_report) = SpeechScorer::from_bytes(&bytes, SileroOptions::default()).unwrap();
    let coreml = SileroOptions {
        execution_provider: ExecutionProvider::CoreMl,
        ..SileroOptions::default()
    };
    let (mut ml, ml_report) = SpeechScorer::from_bytes(&bytes, coreml).unwrap();
    assert_eq!(cpu_report.backend, Backend::SileroCpu);
    assert_eq!(
        ml_report,
        ScorerBackend {
            backend: Backend::SileroCoreMl,
            fallback_reason: None,
        }
    );
    let a = cpu.scores(&audio).unwrap();
    let b = ml.scores(&audio).unwrap();
    assert_eq!(a.len(), b.len());
    let max_diff = a
        .iter()
        .zip(&b)
        .map(|(x, y)| (x - y).abs())
        .fold(0f32, f32::max);
    assert!(
        max_diff < 0.05,
        "CoreML scores diverge from CPU: {max_diff}"
    );
}

#[test]
fn corrupt_model_bytes_fall_back_to_energy_with_reason() {
    need_ort!();
    init_ort_runtime();
    let junk = b"not an onnx model";
    assert!(SpeechScorer::from_bytes(junk, SileroOptions::default()).is_err());
    let fallback = SileroOptions {
        energy_fallback: true,
        ..SileroOptions::default()
    };
    let (scorer, report) = SpeechScorer::from_bytes(junk, fallback).unwrap();
    assert!(matches!(scorer, SpeechScorer::Energy));
    assert_eq!(report.backend, Backend::Energy);
    assert!(report.fallback_reason.is_some());
}

/// Warm-up divergence: `KeepState` keeps the state the zero warm-up frame
/// produced, so the first real frames score differently from `ResetAfter`,
/// and the difference fades as the state adapts.
#[test]
fn keep_state_warmup_diverges_early_and_converges() {
    let model = need_model!();
    init_ort_runtime();
    let bytes = std::fs::read(&model).unwrap();
    let audio = load_wav();
    let mut reset = Silero::from_bytes(&bytes, SileroOptions::default()).unwrap();
    let keep_opts = SileroOptions {
        warmup: Warmup::KeepState,
        ..SileroOptions::default()
    };
    let mut keep = Silero::from_bytes(&bytes, keep_opts).unwrap();
    let mut early_diff = 0f32;
    let mut max_diff = 0f32;
    for (i, c) in audio.as_chunks::<FRAME_SAMPLES>().0.iter().enumerate() {
        let d = (reset.score(c).unwrap() - keep.score(c).unwrap()).abs();
        if i < 10 {
            early_diff = early_diff.max(d);
        }
        max_diff = max_diff.max(d);
    }
    assert!(
        early_diff > 0.0,
        "KeepState must differ on the first frames"
    );
    assert!(
        max_diff < 0.1,
        "warm-up divergence did not converge: {max_diff}"
    );
}
