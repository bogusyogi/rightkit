//! Shipping-path Parakeet eval: goes through `transcribe_padded_window` exactly
//! as the app does, instead of calling the raw `CoreMlParakeet::transcribe`
//! primitive.
//!
//! WHY: `examples/parakeet_eval.rs` calls `model.transcribe()` directly, which
//! lands in `decode_all` — hard, fixed 15 s chunks with no boundary logic. That
//! is the low-level primitive; the SuperWhisper-matched padded window (quiet cut
//! in the last 2.24 s, 16-char overlap dedup) lives one layer up in
//! `AsrRuntime`. Measured on the canonical corpus, the raw primitive dropped 33
//! of 316 words on a 110 s clip because cuts landed mid-word, which made macOS
//! look 0.67 WER points worse than iOS. This runner measures what ships.
//!
//!   cargo run --release --example parakeet_eval_shipping -- <model-dir> <wav-dir>
#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn load_wav(path: &std::path::Path) -> Vec<f32> {
    let wav = rightkit_audio::wav::read_wav_mono_f32(path)
        .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    wav.samples
}

#[cfg(target_os = "macos")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: parakeet_eval_shipping <model-dir> <wav-dir> [final|text]");
        std::process::exit(2);
    }
    // `transcribe` short-circuits audio within one window to
    // `CoreMlParakeet::transcribe` -> `detok`, but production dictation
    // finalises through `transcribe_final_result_under_lease` ->
    // `coreml_parakeet_result` -> `transcribe_pieces_timed`, whose assembly
    // differs. Measuring the former while shipping the latter is how a digit
    // join defect survived this gate; see ASR_DECODE_CONTRACT.md sections 4 and
    // 6. Default to the path that actually ships.
    let use_final_path = args.get(3).map(|m| m != "text").unwrap_or(true);
    eprintln!(
        "path: {}",
        if use_final_path {
            "transcribe_final_result_under_lease (production dictation)"
        } else {
            "transcribe (text-only; short clips bypass the production assembly)"
        }
    );
    // Construct the runtime variant directly: `AsrRuntime::load` resolves the
    // backend from user settings, which an eval must not depend on.
    let model =
        rightkit_dictation_engine::coreml_asr::CoreMlParakeet::load(std::path::Path::new(&args[1]))
            .expect("load CoreML Parakeet bundle");
    let mut runtime = rightkit_dictation_engine::asr::AsrRuntime::CoreMlParakeet(model);

    let mut wavs: Vec<_> = std::fs::read_dir(&args[2])
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "wav").unwrap_or(false))
        .collect();
    wavs.sort();

    // Warm: first decode pays model load / ANE program setup.
    if let Some(first) = wavs.first() {
        let audio = load_wav(first);
        if !audio.is_empty() {
            let _ = if use_final_path {
                runtime
                    .transcribe_final_result_under_lease(&audio)
                    .map(|r| r.text)
            } else {
                runtime.transcribe(&audio)
            };
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
        let text = if use_final_path {
            runtime
                .transcribe_final_result_under_lease(&audio)
                .map(|r| r.text)
        } else {
            runtime.transcribe(&audio)
        }
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
