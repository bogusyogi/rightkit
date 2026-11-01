//! Windows/ONNX shipping-path Parakeet eval.
//!
//! The macOS counterpart is `parakeet_eval_shipping`, which is CoreML-only. The
//! ONNX path had no eval harness at all, so nothing about Windows decode quality
//! had ever been measured — including `RIGHTKIT_TDT_DURATION_CAP`, whose default of 2
//! clamps frame-skip to half what macOS allows.
//!
//! Goes through `AsrRuntime::transcribe_final_result_under_lease`, the same call
//! production dictation finalises with, not a lower-level primitive.
//!
//!   cargo run -p rightkit-dictation-inference --release --example parakeet_eval_onnx \
//!       -- <model-dir> <wav-dir> [dml|cpu]
//!
//! Lives in `rightkit-dictation-inference`, not `rightkit-dictation-engine`, because
//! `rightkit-dictation-runtime` does not currently compile on Windows
//! (`windows_audio_lifecycle.rs`, pre-existing, unrelated to ASR) and
//! `rightkit-dictation-engine` depends on it. `rightkit-dictation-inference` builds cleanly on
//! Windows and already owns `AsrRuntime`, so the harness has no reason to sit
//! behind that break.
//!
//! Honour `RIGHTKIT_TDT_DURATION_CAP` / `RIGHTKIT_ASR_BEAM_WIDTH` from the environment to
//! compare policies without rebuilding.
#[cfg(target_os = "macos")]
fn main() {}

#[cfg(not(target_os = "macos"))]
fn load_wav(path: &std::path::Path) -> Vec<f32> {
    let wav = rightkit_audio::wav::read_wav_mono_f32(path)
        .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    wav.samples
}

#[cfg(not(target_os = "macos"))]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: parakeet_eval_onnx <model-dir> <wav-dir> [dml|cpu]");
        std::process::exit(2);
    }
    #[cfg(target_os = "windows")]
    let ep = match args.get(3).map(|s| s.as_str()) {
        Some("cpu") => rightkit_dictation_inference::asr::AsrEp::Cpu,
        _ => rightkit_dictation_inference::asr::AsrEp::Dml,
    };
    #[cfg(not(target_os = "windows"))]
    let ep = rightkit_dictation_inference::asr::AsrEp::Cpu;

    eprintln!(
        "duration_cap={} beam_width={}",
        std::env::var("RIGHTKIT_TDT_DURATION_CAP").unwrap_or_else(|_| "2 (default)".into()),
        // NOT greedy. This harness runs `transcribe_final_result_under_lease`,
        // which takes the first-party beam at width 2 unless overridden.
        std::env::var("RIGHTKIT_ASR_BEAM_WIDTH").unwrap_or_else(|_| "2 (default)".into()),
    );

    let mut runtime =
        rightkit_dictation_inference::asr::AsrRuntime::load(std::path::Path::new(&args[1]), ep)
            .expect("load ONNX Parakeet");

    let mut wavs: Vec<_> = std::fs::read_dir(&args[2])
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "wav").unwrap_or(false))
        .collect();
    wavs.sort();

    if let Some(first) = wavs.first() {
        let audio = load_wav(first);
        if !audio.is_empty() {
            let _ = runtime.transcribe_final_result_under_lease(&audio);
        }
    }

    let (mut total_audio, mut total_decode) = (0f64, 0f64);
    for wav in &wavs {
        let audio = load_wav(wav);
        if audio.is_empty() {
            continue;
        }
        let seconds = audio.len() as f64 / 16_000.0;
        let started = std::time::Instant::now();
        let text = runtime
            .transcribe_final_result_under_lease(&audio)
            .map(|r| r.text)
            .unwrap_or_else(|e| format!("<err {e}>"));
        let elapsed = started.elapsed().as_secs_f64();
        total_audio += seconds;
        total_decode += elapsed;
        println!(
            "\n{} [{:.1}s audio, {:.0}ms decode, RTF {:.3}]:\n  {text}",
            wav.file_name().unwrap().to_string_lossy(),
            seconds,
            elapsed * 1000.0,
            elapsed / seconds
        );
    }
    println!(
        "\n=== TOTAL: {:.1}s audio, {:.2}s decode, RTF {:.3} ===",
        total_audio,
        total_decode,
        total_decode / total_audio
    );
}
