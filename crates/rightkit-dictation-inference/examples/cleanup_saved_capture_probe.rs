//! Replay saved Windows control captures through production final decode and cleanup.
//!
//! Input JSON: {"rows":[{"id":"session-16","path":"...wav",
//! "onset_sample":123,"intent":"send"}]}
//! Run through RightKit on Windows:
//! `cargo run --example cleanup_saved_capture_probe -- MODEL_ROOT MANIFEST OUTPUT.jsonl`

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("cleanup_saved_capture_probe requires Windows");
    std::process::exit(2);
}

#[cfg(target_os = "windows")]
fn main() -> Result<(), String> {
    use rightkit_dictation_core::audio_conditioning::condition_for_asr;
    use rightkit_dictation_inference::{
        asr::{AsrEp, AsrRuntime},
        control_cleanup::{
            cleanup_confirmed_control, cleanup_confirmed_control_with_exact_retry, Alignment,
        },
    };
    use rightkit_text_polish::text_pipeline::ControlIntent;
    use serde::{Deserialize, Serialize};
    use serde_json::{json, Value};
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    #[derive(Deserialize)]
    struct Manifest {
        rows: Vec<Row>,
    }
    #[derive(Deserialize)]
    struct Row {
        id: String,
        path: PathBuf,
        onset_sample: usize,
        intent: String,
        #[serde(default)]
        decode_start_sample: usize,
    }
    #[derive(Serialize)]
    struct Token {
        text: String,
        start: f32,
        end: f32,
    }

    fn read_wav(path: &Path) -> Result<Vec<f32>, String> {
        let wav = rightkit_audio::wav::read_wav_mono_f32(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if wav.sample_rate != 16_000_u32 {
            return Err(format!(
                "{}: expected {} Hz WAV, got {} Hz",
                path.display(),
                16_000,
                wav.sample_rate
            ));
        }
        Ok(wav.samples)
    }

    fn intent(value: &str) -> Result<ControlIntent, String> {
        match value {
            "send" | "zephyr send" => Ok(ControlIntent::Send),
            "stop" | "zephyr stop" => Ok(ControlIntent::Stop),
            "cancel" | "zephyr cancel" => Ok(ControlIntent::Cancel),
            other => Err(format!("unsupported intent {other}")),
        }
    }

    fn alignment(value: Result<Alignment, String>) -> Value {
        match value {
            Ok(value) => json!({
                "boundary_sec": value.boundary_sec,
                "accepted": value.accepted,
                "method": value.method,
                "score": value.score,
                "joint_calls": value.joint_calls,
            }),
            Err(error) => json!({"error": error}),
        }
    }

    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: cleanup_saved_capture_probe MODEL_ROOT MANIFEST OUTPUT.jsonl".into());
    }
    let model_root = PathBuf::from(&args[0]);
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(&args[1]).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    if std::env::var_os("ORT_DYLIB_PATH").is_none() {
        return Err("set ORT_DYLIB_PATH to installed runtime".into());
    }
    std::env::set_var("RIGHTKIT_ASR_BACKEND", "parakeet-tdt");
    let mut runtime = AsrRuntime::load(&model_root, AsrEp::Dml)?;
    if !runtime.supports_control_cleanup() {
        return Err("loaded runtime does not support control cleanup".into());
    }
    let mut output = fs::File::create(&args[2]).map_err(|error| error.to_string())?;
    for row in manifest.rows {
        let audio = read_wav(&row.path)?;
        if row.onset_sample == 0 || row.onset_sample > audio.len() {
            return Err(format!("{} has invalid onset_sample", row.id));
        }
        let command_intent = intent(&row.intent)?;
        let conditioned = condition_for_asr(&audio[row.decode_start_sample..], 16_000, "default");
        let mut full = runtime.transcribe_final_result_under_lease(&conditioned)?;
        let offset = row.decode_start_sample as f32 / 16_000.0;
        for token in &mut full.tokens {
            token.start += offset;
            token.end += offset;
        }
        let command = match command_intent {
            ControlIntent::Send => "zephyr send",
            ControlIntent::Stop => "zephyr stop",
            ControlIntent::Cancel => "zephyr cancel",
        };
        let start = audio.len().saturating_sub(14 * 16_000);
        let end = row.onset_sample.saturating_add(2 * 16_000).min(audio.len());
        let alignment_audio = condition_for_asr(&audio[start..end], 16_000, "default");
        let earliest = (row.onset_sample - start) as f32 / 16_000.0 - 1.0;
        let fast = runtime.align_control_suffix_under_lease(&alignment_audio, command, earliest);
        let exact =
            runtime.align_control_suffix_exact_under_lease(&alignment_audio, command, earliest);
        let before = cleanup_confirmed_control(
            full.clone(),
            &audio,
            command_intent,
            row.onset_sample,
            |samples, command, earliest| {
                runtime.align_control_suffix_under_lease(samples, command, earliest)
            },
        );
        let cleanup = cleanup_confirmed_control_with_exact_retry(
            full.clone(),
            &audio,
            command_intent,
            row.onset_sample,
            |samples, command, earliest, exact_only| {
                if exact_only {
                    runtime.align_control_suffix_exact_under_lease(samples, command, earliest)
                } else {
                    runtime.align_control_suffix_under_lease(samples, command, earliest)
                }
            },
        );
        let tokens: Vec<Token> = full
            .tokens
            .iter()
            .map(|token| Token {
                text: token.text.clone(),
                start: token.start,
                end: token.end,
            })
            .collect();
        let raw_text = full.text.clone();
        let record = json!({
            "id": row.id,
            "path": row.path,
            "audio_samples": audio.len(),
            "onset_sample": row.onset_sample,
            "decode_start_sample": row.decode_start_sample,
            "raw_text": raw_text,
            "raw_tokens": tokens,
            "fast_alignment": alignment(fast),
            "exact_alignment": alignment(exact),
            "before_fast_only": {"text": before.transcript.text, "boundary_sec": before.boundary_sec, "method": before.method},
            "cleanup": {
                "text": cleanup.transcript.text,
                "method": cleanup.method,
                "boundary_sec": cleanup.boundary_sec,
                "score": cleanup.score,
                "joint_calls": cleanup.joint_calls,
                "error": cleanup.error,
            },
        });
        println!("{}", record);
        serde_json::to_writer(&mut output, &record).map_err(|error| error.to_string())?;
        use std::io::Write;
        output.write_all(b"\n").map_err(|error| error.to_string())?;
    }
    Ok(())
}
