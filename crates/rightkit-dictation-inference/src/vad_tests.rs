use super::{SpeechVad, VadStatus, FRAME_SAMPLES};
// Only the macOS bundling test names these, so importing them unconditionally
// is an unused-import warning on every other platform.
#[cfg(target_os = "macos")]
use super::{model_path_candidates, MODEL_FILE};

#[cfg(target_os = "macos")]
#[test]
fn bundled_macos_default_selects_the_onnx_model() {
    let candidates = model_path_candidates();
    assert!(!candidates.is_empty());
    assert!(candidates
        .iter()
        .all(|path| path.file_name().and_then(|name| name.to_str()) == Some(MODEL_FILE)));
    assert!(candidates
        .iter()
        .all(|path| path.extension().and_then(|ext| ext.to_str()) == Some("onnx")));
}

struct Recorder(std::rc::Rc<std::cell::RefCell<Vec<f32>>>);

impl rightkit_vad::FrameScorer for Recorder {
    fn reset(&mut self) {}
    fn score(&mut self, frame: &[f32; FRAME_SAMPLES]) -> rightkit_vad::Result<f32> {
        self.0.borrow_mut().extend_from_slice(frame);
        Ok(0.0)
    }
}

#[test]
fn chunked_capture_preserves_every_sample_across_silero_frames() {
    let input: Vec<f32> = (0..(FRAME_SAMPLES * 3 + 137))
        .map(|sample| sample as f32)
        .collect();
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut stream = rightkit_vad::FrameStream::new(Recorder(seen.clone()), 0.5);

    for chunk in input.chunks(700) {
        stream.push(chunk).expect("recorder never fails");
    }

    assert_eq!(*seen.borrow(), input[..FRAME_SAMPLES * 3]);
}

#[test]
fn missing_model_does_not_confirm_speech() {
    let mut vad = SpeechVad::with_model_path(None);
    assert!(!vad.observe(&vec![0.8; FRAME_SAMPLES]));
    assert_eq!(vad.status(), VadStatus::Failed);
    assert!(!vad.is_ready());
}

#[test]
#[ignore = "provisioned: needs the staged ONNX Runtime (src-tauri/resources/runtime), which is not tracked"]
fn silence_does_not_confirm_speech() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,rightkit_dictation_inference=info")
        .with_writer(std::io::stderr)
        .try_init();
    let mut vad = SpeechVad::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    while std::time::Instant::now() < deadline {
        assert!(!vad.observe(&vec![0.0; FRAME_SAMPLES]));
        if vad.is_ready_for_test() {
            return;
        }
    }
    assert!(vad.is_ready_for_test(), "Silero VAD did not become ready");
}

use super::{default_model_path, ort_dylib_path};

/// 16 kHz mono f32 from a repo fixture, converted exactly as the file path does
/// (i16 / 32768). `None` (with a reason) when the fixture is not 16 kHz mono.
fn fixture_16k_mono(name: &str) -> Option<Vec<f32>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rightkit-dictation-engine/tests/fixtures")
        .join(name);
    let wav = rightkit_audio::wav::read_wav_mono_f32(&path).ok()?;
    if wav.sample_rate != 16_000 {
        eprintln!("SKIP: {name} is not 16 kHz ({} Hz)", wav.sample_rate);
        return None;
    }
    Some(wav.samples)
}

// ---------------------------------------------------------------------------
// Chunking invariance: irregular capture callbacks must yield exactly the same
// per-frame verdicts and aggregate as exact 512-sample feeding.
// ---------------------------------------------------------------------------

fn ready_vad() -> Option<SpeechVad> {
    let (Some(_), Some(_)) = (default_model_path(), ort_dylib_path()) else {
        eprintln!("SKIP: silero model or ORT runtime not found");
        return None;
    };
    let mut vad = SpeechVad::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if vad.is_ready() {
            return Some(vad);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    eprintln!("SKIP: Silero VAD did not become ready");
    None
}

#[test]
fn irregular_chunks_match_exact_frame_feeding() {
    let (Some(mut exact), Some(mut chunked)) = (ready_vad(), ready_vad()) else {
        return;
    };
    // Deterministic pseudo-random noise bursts, silence, then real speech if available.
    let mut seed = 0x1234_5678_u32;
    let mut noise = |n: usize, amp: f32| -> Vec<f32> {
        (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * amp
            })
            .collect()
    };
    let mut signal = noise(16_000, 0.4);
    signal.extend(std::iter::repeat_n(0.0, 8_000));
    signal.extend(noise(9_001, 0.05));
    if let Some(speech) = fixture_16k_mono("short_9s.wav") {
        signal.extend(speech);
    }
    signal.extend(std::iter::repeat_n(0.0, 5_000));

    let mut expected = Vec::new();
    for frame in signal.as_chunks::<FRAME_SAMPLES>().0 {
        expected.extend(exact.observe_frames(frame).expect("exact feed"));
    }
    let sizes = [1usize, 511, 512, 513, 1000, 7, 4096];
    let (mut actual, mut offset, mut turn) = (Vec::new(), 0usize, 0usize);
    while offset < signal.len() {
        let end = (offset + sizes[turn % sizes.len()]).min(signal.len());
        actual.extend(
            chunked
                .observe_frames(&signal[offset..end])
                .expect("chunked feed"),
        );
        offset = end;
        turn += 1;
    }
    assert_eq!(expected.len(), signal.len() / FRAME_SAMPLES);
    assert_eq!(actual, expected, "per-frame verdicts differ");
    assert_eq!(chunked.recording_aggregate(), exact.recording_aggregate());
    assert_eq!(
        exact.recording_aggregate(),
        Some((
            expected.len() as u64,
            expected.iter().filter(|s| **s).count() as u64
        ))
    );
    eprintln!(
        "chunking: {} frames, aggregate {:?}",
        expected.len(),
        exact.recording_aggregate()
    );
}

#[test]
fn default_session_options_keep_rightkit_thread_and_warmup_policy() {
    let overridden = [
        "RIGHTKIT_VAD_ORT_THREADS",
        "RIGHTKIT_VAD_ORT_INTRA_THREADS",
        "RIGHTKIT_VAD_ORT_INTER_THREADS",
        "RIGHTKIT_VAD_ORT_INTRA_SPINNING",
        "RIGHTKIT_VAD_ORT_INTER_SPINNING",
        "RIGHTKIT_VAD_ORT_INDEPENDENT_POOL",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some());
    if overridden {
        eprintln!("SKIP: RIGHTKIT_VAD_ORT_* probe variables are set");
        return;
    }
    let options = super::session_options();
    assert_eq!((options.intra_threads, options.inter_threads), (1, 1));
    assert!(matches!(options.warmup, rightkit_vad::Warmup::KeepState));
    assert_eq!(options.own_thread_pool, Some(cfg!(target_os = "windows")));
    let expected = if cfg!(target_os = "windows") {
        rightkit_vad::Spinning::Off
    } else {
        rightkit_vad::Spinning::RuntimeDefault
    };
    assert_eq!(options.intra_op_spinning, expected);
    assert_eq!(options.inter_op_spinning, expected);
}
