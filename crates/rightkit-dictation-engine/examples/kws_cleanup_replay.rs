//! Replay frozen ASR outputs through production confirmed-control cleanup.
//! Inputs contain only id/path/command/onset/full_text/tokens; score separately.
#[cfg(target_os = "macos")]
fn main() -> Result<(), String> {
    use rightkit_dictation_inference::{
        control_cleanup::{cleanup_confirmed_control, Alignment},
        coreml_asr::CoreMlParakeet,
        TimedToken, TranscriptionResult,
    };
    use rightkit_text_polish::text_pipeline::ControlIntent;
    use serde_json::{json, Value};
    use std::{fs, io::Write, path::Path, time::Instant};
    let args: Vec<_> = std::env::args().skip(1).collect();
    let transcribe = args.get(3).is_some_and(|arg| arg == "--transcribe");
    if args.len() != 3 && !(args.len() == 4 && transcribe) {
        return Err(
            "usage: kws_cleanup_replay MODEL INPUTS_JSONL OUTPUT_JSONL [--transcribe]".into(),
        );
    }
    let rows: Vec<Value> = fs::read_to_string(&args[1])
        .map_err(|e| e.to_string())?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    let model = CoreMlParakeet::load(Path::new(&args[0]))?;
    let mut runtime = rightkit_dictation_inference::asr::AsrRuntime::CoreMlParakeet(model);
    println!(
        "event=model_loaded elapsed_ms={}",
        started.elapsed().as_millis()
    );
    let mut output = fs::File::create(&args[2]).map_err(|e| e.to_string())?;
    for (index, row) in rows.iter().enumerate() {
        let path = row["path"].as_str().ok_or("missing path")?;
        let wav = rightkit_audio::wav::read_wav_mono_f32(std::path::Path::new(path))
            .map_err(|e| e.to_string())?;
        if wav.sample_rate != 16000 {
            return Err("requires16k mono".into());
        }
        let audio: Vec<f32> = wav.samples;
        let intent = match row["command"].as_str().ok_or("command")? {
            "zephyr send" => ControlIntent::Send,
            "zephyr stop" => ControlIntent::Stop,
            "zephyr cancel" => ControlIntent::Cancel,
            _ => return Err("unknown command".into()),
        };
        let onset = row["onset"].as_u64().ok_or("onset")? as usize;
        // Optional fresh native ASR uses production final assembly & conditioning;
        // frozen corpus mode keeps original transcripts/timestamps unchanged.
        let full = if transcribe {
            let conditioned = rightkit_dictation_core::audio_conditioning::condition_for_asr(
                &audio,
                16000,
                "asr_simple_gain_hpf",
            );
            runtime.transcribe_final_result_under_lease(&conditioned)?
        } else {
            TranscriptionResult {
                text: row["full_text"].as_str().ok_or("text")?.into(),
                tokens: row["tokens"]
                    .as_array()
                    .ok_or("tokens")?
                    .iter()
                    .map(|t| {
                        Ok(TimedToken {
                            text: t["text"].as_str().ok_or("token text")?.into(),
                            start: t["start_s"].as_f64().ok_or("token start")? as f32,
                            end: t["end_s"].as_f64().ok_or("token end")? as f32,
                        })
                    })
                    .collect::<Result<_, String>>()?,
            }
        };
        for repeat in 0..2 {
            let full = full.clone();
            let input_text = full.text.clone();
            let began = Instant::now();
            let mut calls = 0;
            let result = cleanup_confirmed_control(
                full,
                &audio,
                intent,
                onset,
                |conditioned, command, earliest| {
                    let rightkit_dictation_inference::asr::AsrRuntime::CoreMlParakeet(model) =
                        &runtime
                    else {
                        return Err("requires CoreML".into());
                    };
                    calls += 1;
                    let aligned = model.align_control_suffix(conditioned, command, earliest)?;
                    Ok(Alignment {
                        boundary_sec: aligned.boundary_sec,
                        accepted: aligned.accepted,
                        method: aligned.method,
                        score: aligned.score,
                        joint_calls: aligned.joint_calls,
                    })
                },
            );
            writeln!(output,"{}",json!({"id":row["id"],"repeat":repeat,"input_text":input_text,"method":result.method,"boundary":result.boundary_sec,"score":result.score,"joint_calls":result.joint_calls,"error":result.error,"align_calls":calls,"clean":result.transcript.text,"elapsed_us":began.elapsed().as_micros()})).map_err(|e|e.to_string())?;
            output.flush().map_err(|e| e.to_string())?;
        }
        println!(
            "event=clip_complete completed={} total={}",
            index + 1,
            rows.len()
        );
    }
    println!("event=complete");
    Ok(())
}
#[cfg(target_os = "windows")]
fn main() -> Result<(), String> {
    use rightkit_dictation_engine::asr::{AsrEp, AsrRuntime};
    use rightkit_dictation_inference::{
        control_cleanup::cleanup_confirmed_control_with_exact_retry, TimedToken,
        TranscriptionResult,
    };
    use rightkit_text_polish::text_pipeline::ControlIntent;
    use serde_json::{json, Value};
    use std::{fs, io::Write, path::Path, time::Instant};

    let args: Vec<_> = std::env::args().skip(1).collect();
    let exact_only = args.get(3).is_some_and(|arg| arg == "--exact-only");
    if args.len() != 3 && !(args.len() == 4 && exact_only) {
        return Err(
            "usage: kws_cleanup_replay MODELS_DIR INPUTS_JSONL OUTPUT_JSONL [--exact-only]".into(),
        );
    }
    let rows: Vec<Value> = fs::read_to_string(&args[1])
        .map_err(|e| e.to_string())?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, serde_json::Error>>()
        .map_err(|e| e.to_string())?;
    let ort = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../src-tauri/resources/runtime/onnxruntime.dll");
    rightkit_onnx::configure_runtime_with_options(
        [rightkit_onnx::RuntimeCandidate::new(
            ort,
            rightkit_onnx::CandidateSource::Explicit,
        )],
        &rightkit_onnx::RuntimeBindingOptions {
            force_replace: true,
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    std::env::set_var("RIGHTKIT_ASR_BACKEND", "parakeet-tdt");
    let started = Instant::now();
    let mut model = AsrRuntime::load(Path::new(&args[0]), AsrEp::Dml)?;
    assert!(
        model.supports_control_cleanup(),
        "loaded Windows ASR runtime does not support control cleanup"
    );
    println!(
        "event=model_loaded elapsed_ms={}",
        started.elapsed().as_millis()
    );
    let mut output = fs::File::create(&args[2]).map_err(|e| e.to_string())?;
    for (index, row) in rows.iter().enumerate() {
        let path = row["path"].as_str().ok_or("missing path")?;
        let wav = rightkit_audio::wav::read_wav_mono_f32(std::path::Path::new(path))
            .map_err(|e| e.to_string())?;
        if wav.sample_rate != 16000 {
            return Err("requires16k mono".into());
        }
        let audio: Vec<f32> = wav.samples;
        let intent = match row["command"].as_str().ok_or("command")? {
            "zephyr send" => ControlIntent::Send,
            "zephyr stop" => ControlIntent::Stop,
            "zephyr cancel" => ControlIntent::Cancel,
            _ => return Err("unknown command".into()),
        };
        let onset = row["onset"].as_u64().ok_or("onset")? as usize;
        for repeat in 0..2 {
            let full = TranscriptionResult {
                text: row["full_text"].as_str().ok_or("text")?.into(),
                tokens: row["tokens"]
                    .as_array()
                    .ok_or("tokens")?
                    .iter()
                    .map(|t| {
                        Ok(TimedToken {
                            text: t["text"].as_str().ok_or("token text")?.into(),
                            start: t["start_s"].as_f64().ok_or("token start")? as f32,
                            end: t["end_s"].as_f64().ok_or("token end")? as f32,
                        })
                    })
                    .collect::<Result<_, String>>()?,
            };
            let began = Instant::now();
            let mut calls = 0;
            let result = cleanup_confirmed_control_with_exact_retry(
                full,
                &audio,
                intent,
                onset,
                |conditioned, command, earliest, exact| {
                    calls += 1;
                    if exact_only || exact {
                        model.align_control_suffix_exact_under_lease(conditioned, command, earliest)
                    } else {
                        model.align_control_suffix_under_lease(conditioned, command, earliest)
                    }
                },
            );
            writeln!(output, "{}", json!({"id":row["id"],"repeat":repeat,"method":result.method,"boundary":result.boundary_sec,"score":result.score,"joint_calls":result.joint_calls,"error":result.error,"align_calls":calls,"clean":result.transcript.text,"elapsed_us":began.elapsed().as_micros()})).map_err(|e| e.to_string())?;
            output.flush().map_err(|e| e.to_string())?;
        }
        println!(
            "event=clip_complete completed={} total={}",
            index + 1,
            rows.len()
        );
    }
    println!("event=complete");
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {
    eprintln!("Native CoreML/ONNX replay requires macOS or Windows");
}
