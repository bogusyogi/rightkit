//! A/B the two VAD backends through the real engine wiring.
//!
//! Runs the same wav(s) through `SpeechVad` twice — once pointed at the
//! CoreML `.mlmodelc`, once at the ONNX model (ORT, bundled dylib) — with
//! identical 512-sample framing, and compares per-frame decisions.
//!
//!   RIGHTKIT_VAD_ORT_DYLIB=<path/to/libonnxruntime.dylib> \
//!     cargo run --release --example vad_parity_check -- <wav> [<wav>...]
//!
//! Wavs must be 16 kHz mono s16le (e.g. adaptive/win_raws/*_raw.wav).
//!
//! macOS no longer bundles `libonnxruntime.dylib` (the CoreML path replaced it),
//! so the ORT side of the A/B needs a dylib supplied via `RIGHTKIT_VAD_ORT_DYLIB` —
//! e.g. from an older installed the app.app, or any ORT build. Without one,
//! this example explains that instead of failing obscurely.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use rightkit_dictation_engine::vad::SpeechVad;

const FRAME: usize = 512;

fn read_wav_16k_mono(path: &str) -> Vec<f32> {
    let wav = rightkit_audio::wav::read_wav_mono_f32(std::path::Path::new(path))
        .unwrap_or_else(|e| panic!("open {}: {e}", path));
    wav.samples
}

fn wait_ready(vad: &mut SpeechVad, label: &str) {
    // Loading is async; poll with silence until the backend answers.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let _ = vad.observe(&vec![0.0; FRAME]);
        if let Some(name) = vad.backend_name() {
            println!("{label}: ready ({name})");
            vad.reset();
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if label == "ort" {
        eprintln!(
            "ort: no ONNX Runtime available — macOS does not bundle the dylib since the \
             CoreML VAD landed. Set RIGHTKIT_VAD_ORT_DYLIB=<libonnxruntime.dylib> to run the A/B."
        );
        std::process::exit(2);
    }
    panic!("{label}: VAD did not become ready");
}

fn main() {
    let wavs: Vec<String> = std::env::args().skip(1).collect();
    assert!(!wavs.is_empty(), "usage: vad_parity_check <wav> [<wav>...]");

    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/vad");
    let coreml = base.join("silero_vad_16k.mlmodelc");
    let onnx = base.join("silero_vad_16k_op15.onnx");
    assert!(coreml.is_dir(), "missing {}", coreml.display());
    assert!(onnx.is_file(), "missing {}", onnx.display());

    let mut vad_a = SpeechVad::with_model_path(Some(coreml));
    let mut vad_b = SpeechVad::with_model_path(Some(onnx));
    wait_ready(&mut vad_a, "coreml");
    wait_ready(&mut vad_b, "ort");

    let (mut frames, mut speech_a, mut speech_b, mut disagree) = (0u64, 0u64, 0u64, 0u64);
    let (mut t_a, mut t_b) = (Duration::ZERO, Duration::ZERO);
    for wav in &wavs {
        let pcm = read_wav_16k_mono(wav);
        vad_a.reset();
        vad_b.reset();
        let mut file_disagree = 0u64;
        for chunk in pcm.as_chunks::<FRAME>().0 {
            let t0 = Instant::now();
            let a = vad_a.observe(chunk);
            t_a += t0.elapsed();
            let t0 = Instant::now();
            let b = vad_b.observe(chunk);
            t_b += t0.elapsed();
            frames += 1;
            speech_a += a as u64;
            speech_b += b as u64;
            if a != b {
                disagree += 1;
                file_disagree += 1;
            }
        }
        println!("{wav}: disagree={file_disagree}");
    }
    println!(
        "TOTAL frames={frames} speech(coreml)={speech_a} speech(ort)={speech_b} disagree={disagree}"
    );
    println!(
        "per-frame: coreml={:.4} ms ort={:.4} ms",
        t_a.as_secs_f64() * 1000.0 / frames as f64,
        t_b.as_secs_f64() * 1000.0 / frames as f64,
    );
    assert_eq!(disagree, 0, "backends disagreed");
    println!("PARITY OK");
}
