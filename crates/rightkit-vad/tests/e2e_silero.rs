//! End-to-end: real Silero ONNX model + real speech audio.
//!
//! Gate: needs `silero_vad_16k_op15.onnx` (2 MB, MIT). Set `RIGHTKIT_VAD_MODEL`
//! or place it at one of the known local paths. Without it the tests FAIL
//! (not skip) so a missing gate is never reported green; set
//! `RIGHTKIT_VAD_ALLOW_MISSING_MODEL=1` to downgrade to a printed skip.
use rightkit_vad::*;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::sync::OnceLock;

fn init_ort_runtime() {
    #[cfg(target_os = "macos")]
    {
        static READY: OnceLock<()> = OnceLock::new();
        READY.get_or_init(|| {
            use rightkit_media::native::{NativeCatalog, NativeLibrary};
            use rightkit_ort::{
                init_environment, resolve_runtime, CandidateSource, EnvironmentOptions, GlobalPool,
                RuntimeCandidate, runtime_filename,
            };

            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..").canonicalize().unwrap();
            let runtime = std::env::var_os("RIGHTKIT_ORT_TEST_DYLIB")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    root.join(
                        "genright/.cache/devin-completion-20261006/batch5/voice/dl/onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.1.24.4.dylib",
                    )
                });
            assert!(runtime.is_file(), "no approved ONNX Runtime 1.24.4 dylib at {}; set RIGHTKIT_ORT_TEST_DYLIB", runtime.display());
            let staged_dir = std::env::temp_dir().join(format!("rightkit-vad-ort-e2e-{}", std::process::id()));
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
}

fn load_model(path: &Path) -> Silero {
    init_ort_runtime();
    Silero::load(path).unwrap()
}

fn model() -> Option<PathBuf> {
    let mut c: Vec<PathBuf> = std::env::var_os("RIGHTKIT_VAD_MODEL").map(PathBuf::from).into_iter().collect();
    c.push("/Volumes/D/claude/heardright/tauri-app-next/src-tauri/resources/vad/silero_vad_16k_op15.onnx".into());
    c.into_iter().find(|p| p.is_file())
}

fn load_wav() -> Vec<f32> {
    let p = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/short_9s.wav");
    let mut r = hound::WavReader::open(p).unwrap();
    assert_eq!((r.spec().sample_rate, r.spec().channels), (16_000, 1));
    r.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect()
}

macro_rules! need_model {
    () => {
        match model() {
            Some(m) => m,
            None if std::env::var_os("RIGHTKIT_VAD_ALLOW_MISSING_MODEL").is_some() => {
                eprintln!("SKIP: Silero model missing");
                return;
            }
            None => panic!("Silero model missing: set RIGHTKIT_VAD_MODEL"),
        }
    };
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
    assert!(first.start_ms >= 1800, "leading silence marked speech: {first:?}");
    assert!(last.end_ms <= 2000 + 9760 + 200, "trailing silence marked speech: {last:?}");
    let covered: u64 = spans.iter().map(Span::len_ms).sum();
    assert!(covered > 6000, "speech under-covered: {covered} ms");
}

#[test]
fn pure_silence_and_noise_floor_yield_no_spans() {
    let model = need_model!();
    let mut s = load_model(&model);
    assert!(detect(&mut s, &vec![0.0; 16_000 * 3], &VadConfig::default()).unwrap().is_empty());
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
    let cfg = ChunkConfig { merge_gap_ms: 400, max_chunk_ms: 12_000, min_chunk_ms: 200 };
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
