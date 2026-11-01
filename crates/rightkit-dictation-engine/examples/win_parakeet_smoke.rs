// Windows Parakeet smoke: decode WAV clips through the SHIPPING final path
// (`AsrRuntime::load` -> conditioning -> `transcribe_final_result_under_lease`),
// so short-utterance regressions show up here before they show up in the pill.
//
// Usage: cargo run --release --example win_parakeet_smoke -- [--json] [--raw] [--tdt] [--joint-bench] <models_dir> <wav-or-dir>...
//   RIGHTKIT_SMOKE_RAW=1       skip `condition_for_asr` (decode raw PCM)
//   RIGHTKIT_SMOKE_EP=cpu|dml  execution provider (default: production resolve)
//   RIGHTKIT_SMOKE_BIAS=1      apply the production utterance bias before each decode
//   RIGHTKIT_SMOKE_GAP_MS=n    idle n ms before each decode (cold/idle GPU path)
//   RIGHTKIT_SMOKE_KEEPWARM_MS=n  during that gap, decode 1 s of silence every n ms (the app's keep-warm)
#[cfg(target_os = "windows")]
fn main() {
    use rightkit_dictation_engine::asr::{AsrEp, AsrRuntime};
    let bundled_ort = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../src-tauri/resources/runtime/onnxruntime.dll");
    assert!(
        bundled_ort.is_file(),
        "bundled ORT missing: {}",
        bundled_ort.display()
    );
    rightkit_onnx::configure_runtime_with_options(
        [rightkit_onnx::RuntimeCandidate::new(
            &bundled_ort,
            rightkit_onnx::CandidateSource::Explicit,
        )],
        &rightkit_onnx::RuntimeBindingOptions {
            force_replace: true,
            ..Default::default()
        },
    )
    .expect("bind bundled ORT");
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    let json = args.iter().any(|arg| arg == "--json");
    let joint_bench = args.iter().any(|arg| arg == "--joint-bench");
    let raw = args.iter().any(|arg| arg == "--raw") || std::env::var("RIGHTKIT_SMOKE_RAW").is_ok();
    if args.iter().any(|arg| arg == "--tdt") {
        std::env::set_var("RIGHTKIT_ASR_BACKEND", "parakeet-tdt");
    }
    args.retain(|arg| {
        arg != "--json" && arg != "--raw" && arg != "--tdt" && arg != "--joint-bench"
    });
    if joint_bench {
        std::env::set_var("RIGHTKIT_PREDICTOR_BENCH", "1");
    }
    let models_dir = args.remove(0);
    let ep = match std::env::var("RIGHTKIT_SMOKE_EP").as_deref() {
        Ok("cpu") => AsrEp::Cpu,
        Ok("dml") => AsrEp::Dml,
        _ => AsrEp::resolve_default(),
    };
    let mut runtime =
        AsrRuntime::load(std::path::Path::new(&models_dir), ep).expect("load AsrRuntime");
    println!("ep={}", ep.as_str());
    let bias = std::env::var("RIGHTKIT_SMOKE_BIAS").is_ok();
    let gap_ms = std::env::var("RIGHTKIT_SMOKE_GAP_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    let keep_warm_ms = std::env::var("RIGHTKIT_SMOKE_KEEPWARM_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    let silence = vec![0.0f32; 16_000];
    let mut wav_paths = Vec::new();
    for source in args {
        let path = std::path::PathBuf::from(&source);
        if path.is_dir() {
            let mut entries = std::fs::read_dir(&path)
                .expect("read wav directory")
                .map(|entry| entry.expect("wav entry").path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "wav"))
                .collect::<Vec<_>>();
            entries.sort();
            wav_paths.extend(
                entries
                    .into_iter()
                    .map(|entry| entry.to_string_lossy().into_owned()),
            );
        } else {
            wav_paths.push(source);
        }
    }
    for wav_path in wav_paths {
        let wav = rightkit_audio::wav::read_wav_mono_f32(std::path::Path::new(&wav_path))
            .expect("open wav");
        assert_eq!(wav.sample_rate, 16_000, "expected 16 kHz wav");
        let mono: Vec<f32> = wav.samples;
        let input = if raw {
            mono.clone()
        } else {
            rightkit_dictation_core::audio_conditioning::condition_for_asr(&mono, 16_000, "default")
        };
        if bias {
            rightkit_dictation_engine::asr::apply_utterance_bias(&mut runtime);
        }
        if let Some(gap) = gap_ms {
            // With a keep-warm period, the idle gap is filled the way a recording dictation is:
            // a 1 s silence decode each time the model has been idle that long.
            let gap = std::time::Duration::from_millis(gap);
            match keep_warm_ms {
                Some(every) => {
                    let every = std::time::Duration::from_millis(every);
                    let until = std::time::Instant::now() + gap;
                    while std::time::Instant::now() + every < until {
                        std::thread::sleep(every);
                        let _ = runtime.transcribe_final_result_under_lease(&silence);
                    }
                    std::thread::sleep(until.saturating_duration_since(std::time::Instant::now()));
                }
                None => std::thread::sleep(gap),
            }
        }
        let started = std::time::Instant::now();
        let result = runtime.transcribe_final_result_under_lease(&input);
        let ms = started.elapsed().as_millis();
        if json {
            match result {
                Ok(r) => println!(
                    "{}",
                    serde_json::json!({
                        "path": wav_path, "audio_samples": mono.len(), "decode_ms": ms,
                        "text": r.text, "tokens": r.tokens.iter().map(|t| serde_json::json!({
                            "text": t.text, "start": t.start, "end": t.end,
                        })).collect::<Vec<_>>(),
                    })
                ),
                Err(e) => println!(
                    "{}",
                    serde_json::json!({
                        "path": wav_path, "audio_samples": mono.len(), "decode_ms": ms,
                        "error": e,
                    })
                ),
            }
            continue;
        }
        match result {
            Ok(r) => println!(
                "{wav_path}: secs={:.2} ms={ms} words={} text={:?}",
                mono.len() as f32 / 16_000.0,
                r.text.split_whitespace().count(),
                r.text
            ),
            Err(e) => println!("{wav_path}: ERROR {e}"),
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("win_parakeet_smoke is Windows-only");
}
