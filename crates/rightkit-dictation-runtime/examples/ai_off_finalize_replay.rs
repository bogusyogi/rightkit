//! Paired local replay through production final ASR + production engine
//! finalization with AI polish disabled. This harness has no capture or shell
//! delivery owner; it stops at the shell transcript handoff.
//!
//! Arguments: <models-dir> <candidate-inputs.jsonl> [output.jsonl] [ort-dll] [dml-adapter-index]
//! On macOS, models-dir is the installed CoreML Parakeet TDT bundle. On
//! Windows, it is the app's shared models root and the production DML path
//! is selected.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[cfg(target_os = "windows")]
use rightkit_dictation_inference::asr::AsrEp;
use rightkit_dictation_inference::asr::AsrRuntime;
use rightkit_dictation_runtime::runtime::{EngineRuntime, FinalizeOutcome};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
struct InputRow {
    id: String,
    audio_path: PathBuf,
    sha256: String,
    duration_s: f64,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("ai_off_finalize_replay: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "--finalizer-only") {
        return run_finalizer_only(&args[1..]);
    }
    #[cfg(target_os = "windows")]
    if !(4..=5).contains(&args.len()) {
        return Err(
            "Windows replay requires explicit output.jsonl, source-matched ort-dll, and optional dml-adapter-index"
                .into(),
        );
    }
    #[cfg(target_os = "macos")]
    if !(2..=4).contains(&args.len()) {
        return Err(
            "usage: ai_off_finalize_replay <models-dir> <candidate-inputs.jsonl> [output.jsonl] [joint2]"
                .into(),
        );
    }
    isolate_ai_polish_off()?;
    #[cfg(target_os = "macos")]
    let hyp_batch_arm = match args.get(3).map(String::as_str) {
        None => "control_unset",
        Some("joint2") => {
            std::env::set_var("RIGHTKIT_COREML_HYP_BATCH", "joint");
            std::env::set_var("RIGHTKIT_COREML_HYP_BATCH_MAX_ROWS", "2");
            "joint_max_rows_2"
        }
        Some(value) => return Err(format!("unsupported macOS hypothesis-batch arm {value:?}")),
    };
    #[cfg(target_os = "windows")]
    let hyp_batch_arm = "control_unset";
    #[cfg(target_os = "windows")]
    let dml_adapter_index = {
        let index = match args.get(4) {
            Some(value) => value
                .parse::<i32>()
                .map_err(|error| format!("parse dml-adapter-index {value:?}: {error}"))?,
            None => 0,
        };
        if index < 0 {
            return Err(format!(
                "dml-adapter-index must be nonnegative, got {index}"
            ));
        }
        // Match normal DirectML default adapter selection (DXGI index 0), while
        // allowing a bounded explicit alternate for controlled experiments.
        rightkit_dictation_inference::calibration::set_active_route(
            rightkit_dictation_inference::calibration::ActiveRoute {
                dml_device_id: Some(index),
            },
        );
        Some(index)
    };
    #[cfg(not(target_os = "windows"))]
    let dml_adapter_index: Option<i32> = None;

    let model_dir = PathBuf::from(&args[0]);
    let manifest_path = PathBuf::from(&args[1]);
    let output = args.get(2).map(PathBuf::from);
    let trace_path = output
        .as_ref()
        .map(|path| path.with_extension("trace.log"))
        .unwrap_or_else(|| PathBuf::from("ai_off_finalize_replay.trace.log"));
    let trace_file = std::fs::File::create(&trace_path)
        .map_err(|error| format!("create trace log {}: {error}", trace_path.display()))?;
    let _ = tracing_subscriber::fmt()
        .with_target(true)
        .with_ansi(false)
        .with_writer(trace_file)
        .try_init();
    let manifest = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("read candidate manifest: {error}"))?;
    let rows = manifest
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<InputRow>(line)
                .map_err(|error| format!("parse candidate manifest row: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if rows.is_empty() {
        return Err("candidate manifest is empty".into());
    }

    let load_started = Instant::now();
    #[cfg(target_os = "macos")]
    let mut asr = {
        let model = rightkit_dictation_inference::coreml_asr::CoreMlParakeet::load(&model_dir)
            .map_err(|error| format!("load CoreML Parakeet TDT: {error}"))?;
        AsrRuntime::CoreMlParakeet(model)
    };
    #[cfg(target_os = "windows")]
    let mut asr = {
        let bundled_ort = Path::new(&args[3]);
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
        .map_err(|error| format!("bind the source-matched ONNX Runtime: {error}"))?;
        AsrRuntime::load(&model_dir, AsrEp::Dml)
            .map_err(|error| format!("load DirectML Parakeet TDT: {error}"))?
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    return Err("this replay supports native macOS or Windows only".into());
    let model_load_ms = load_started.elapsed().as_secs_f64() * 1_000.0;

    // Match production worker warmup before the measured rows.
    let warmup_audio = load_wav(&rows[0].audio_path)?;
    let asr_warmup_started = Instant::now();
    asr.transcribe_final_result_under_lease(&warmup_audio)
        .map_err(|error| format!("unscored ASR warmup failed: {error}"))?;
    let asr_warmup_ms = asr_warmup_started.elapsed().as_secs_f64() * 1_000.0;
    let polish_warmup_started = Instant::now();
    rightkit_dictation_text_pipeline::text_polish::warm();
    let mut engine = EngineRuntime::new(model_dir.clone());
    let warmup_session_id = "asr-off-finalizer-warmup";
    engine
        .start_dictation_warmed(warmup_session_id)
        .map_err(|error| format!("start engine finalizer warmup: {error}"))?;
    engine
        .begin_stop(warmup_session_id, false, false)
        .map_err(|error| format!("begin engine finalizer warmup stop: {error}"))?;
    let warmup_outcome = engine.finalize_transcript_with_audio_secs(
        warmup_session_id,
        Ok("Ordinary dictation finalizer warmup.".to_owned()),
        Some(1.0),
    );
    let finalizer_warmup_ms = polish_warmup_started.elapsed().as_secs_f64() * 1_000.0;
    if !matches!(
        &warmup_outcome,
        Ok(FinalizeOutcome::Transcript {
            context_request_attempted: Some(false),
            cancelled: false,
            ..
        })
    ) {
        return Err(format!(
            "unscored engine finalizer warmup did not return ordinary Transcript: {warmup_outcome:?}"
        ));
    }

    let mut writer: Box<dyn std::io::Write> = match output {
        Some(path) => Box::new(
            std::fs::File::create(path).map_err(|error| format!("create output JSONL: {error}"))?,
        ),
        None => Box::new(std::io::stdout().lock()),
    };
    writeln_json(
        &mut writer,
        &json!({
            "event": "run_config",
            "model_load_ms": model_load_ms,
            "asr_warmup_ms": asr_warmup_ms,
            "finalizer_warmup_ms": finalizer_warmup_ms,
            "trace_log": trace_path,
            "backend": if cfg!(target_os = "macos") { "parakeet-tdt-coreml" } else { "parakeet-tdt-directml" },
            "execution_provider": if cfg!(target_os = "windows") { "dml" } else { "coreml" },
            "dml_adapter_index": dml_adapter_index,
            "coreml_hypothesis_batch_arm": hyp_batch_arm,
            "RIGHTKIT_COREML_HYP_BATCH": if hyp_batch_arm == "joint_max_rows_2" { Some("joint") } else { None },
            "RIGHTKIT_COREML_HYP_BATCH_MAX_ROWS": if hyp_batch_arm == "joint_max_rows_2" { Some("2") } else { None },
            "dml_adapter_identity_source": if cfg!(target_os = "windows") { Some("production ASR model loaded trace event") } else { None },
            "ort_runtime_path": if cfg!(target_os = "windows") { Some(args[3].as_str()) } else { None },
            "beam_width": 2,
            "audio_input": "upstream_conditioned_16khz_mono_pcm; no conditioning in this harness",
            "effective_settings": {
                "asr_backend": "parakeet-tdt",
                "dictation_language": "auto",
                "transcript_cleanup": "clean",
                "ai_polish_enabled": false,
                "ai_polish_mode": "app",
                "is_pro": true,
                "audio_conditioning": false,
                "source": "process-local SettingsBlob override"
            },
            "ai_polish_enabled": false,
            "RIGHTKIT_L3_CLEANUP": false,
            "RIGHTKIT_L3_CLOUD_CONSENT": false,
            "provider_keys": "cleared in process; legacy env variables removed without reading",
            "capture": "excluded",
            "delivery_or_paste": "excluded; stop at FinalizeOutcome::Transcript shell handoff"
        }),
    )?;

    for row in rows {
        let item_span = tracing::info_span!(
            target: "rightkit_ai_off_replay",
            "replay_item",
            id = %row.id,
            duration_s = row.duration_s,
            audio_path = %row.audio_path.display()
        );
        let _entered = item_span.enter();
        let read_started = Instant::now();
        let audio = match load_wav(&row.audio_path) {
            Ok(audio) => audio,
            Err(error) => {
                writeln_json(&mut writer, &error_row(row, error, "wav_read"))?;
                continue;
            }
        };
        let wav_read_ms = read_started.elapsed().as_secs_f64() * 1_000.0;
        let actual_duration_s = audio.len() as f64 / 16_000.0;

        let asr_started = Instant::now();
        let asr_result = match asr.transcribe_final_result_under_lease(&audio) {
            Ok(result) => result,
            Err(error) => {
                let total_ms = asr_started.elapsed().as_secs_f64() * 1_000.0;
                let result = json!({
                    "event": "replay_row", "id": row.id, "audio_sha256": row.sha256,
                    "duration_s": actual_duration_s, "status": "error", "error_stage": "asr",
                    "error": error, "wav_read_ms": wav_read_ms,
                    "pure_asr_total_ms": total_ms, "engine_finalize_ms": null,
                    "asr_plus_finalize_total_ms": null, "conditioning_ms": null,
                    "provider_request_attempted": false
                });
                writeln_json(&mut writer, &result)?;
                continue;
            }
        };
        let pure_asr_total_ms = asr_started.elapsed().as_secs_f64() * 1_000.0;
        let pure_asr_timed_tokens = asr_result
            .tokens
            .iter()
            .map(|token| {
                json!({ "text": token.text.as_str(), "start": token.start, "end": token.end })
            })
            .collect::<Vec<_>>();
        let raw_text = asr_result.text;

        // The production phase-1 classifier may dispatch OS commands. Preflight
        // with the exact product classifier and exclude non-dictation routes.
        // AI-transform tails are also excluded because bare summarize can use
        // the system clipboard for selection lookup.
        let command_route =
            rightkit_dictation_command_host::command_classify::classify_action(&raw_text, true);
        let transform_route =
            rightkit_text_polish::text_pipeline::parse_ai_transform_command(&raw_text).is_some()
                || rightkit_text_polish::text_pipeline::is_bare_summarize_trigger(&raw_text);
        if command_route.is_some() || transform_route {
            writeln_json(
                &mut writer,
                &json!({
                    "event": "replay_row", "id": row.id, "audio_sha256": row.sha256,
                    "duration_s": actual_duration_s, "status": "skipped_nonordinary_route",
                    "reason": if command_route.is_some() { "standalone_command" } else { "ai_transform" },
                    "pure_asr_text": raw_text, "pure_asr_timed_tokens": pure_asr_timed_tokens,
                    "pure_asr_total_ms": pure_asr_total_ms,
                    "engine_finalize_ms": null, "asr_plus_finalize_total_ms": null,
                    "conditioning_ms": null, "provider_request_attempted": false
                }),
            )?;
            continue;
        }

        let session_id = format!("asr-off-{}", row.id);
        engine
            .start_dictation_warmed(&session_id)
            .map_err(|error| format!("start engine replay session: {error}"))?;
        engine
            .begin_stop(&session_id, false, false)
            .map_err(|error| format!("begin engine replay stop: {error}"))?;
        let finalize_started = Instant::now();
        let outcome = engine.finalize_transcript_with_audio_secs(
            &session_id,
            Ok(raw_text.clone()),
            Some(actual_duration_s as f32),
        );
        let engine_finalize_ms = finalize_started.elapsed().as_secs_f64() * 1_000.0;
        match outcome {
            Ok(FinalizeOutcome::Transcript {
                text,
                raw_text: finalized_raw,
                context_request_attempted: Some(false),
                cancelled: false,
                ..
            }) => {
                writeln_json(
                    &mut writer,
                    &json!({
                        "event": "replay_row", "id": row.id, "audio_sha256": row.sha256,
                        "duration_s": actual_duration_s, "status": "ok",
                        "pure_asr_text": raw_text, "pure_asr_timed_tokens": pure_asr_timed_tokens,
                        "engine_final_text": text,
                        "engine_raw_text": finalized_raw,
                        "wav_read_ms": wav_read_ms,
                        "pure_asr_total_ms": pure_asr_total_ms,
                        "engine_finalize_ms": engine_finalize_ms,
                        "asr_plus_finalize_total_ms": pure_asr_total_ms + engine_finalize_ms,
                        "conditioning_ms": null,
                        "ai_polish_enabled": false,
                        "cleanup_enabled": false,
                        "cloud_consent": false,
                        "provider_request_attempted": false,
                        "capture_timing": null,
                        "delivery_or_paste_timing": null
                    }),
                )?;
            }
            Ok(other) => writeln_json(
                &mut writer,
                &json!({
                    "event": "replay_row", "id": row.id, "audio_sha256": row.sha256,
                    "duration_s": actual_duration_s, "status": "error",
                    "error_stage": "engine_finalize_outcome",
                    "error": format!("unexpected finalize result: {other:?}"),
                    "pure_asr_text": raw_text, "pure_asr_total_ms": pure_asr_total_ms,
                    "engine_finalize_ms": engine_finalize_ms,
                    "provider_request_attempted": false
                }),
            )?,
            Err(error) => writeln_json(
                &mut writer,
                &json!({
                    "event": "replay_row", "id": row.id, "audio_sha256": row.sha256,
                    "duration_s": actual_duration_s, "status": "error",
                    "error_stage": "engine_finalize", "error": error,
                    "pure_asr_text": raw_text, "pure_asr_total_ms": pure_asr_total_ms,
                    "engine_finalize_ms": engine_finalize_ms,
                    "provider_request_attempted": false
                }),
            )?,
        }
    }
    Ok(())
}

/// Replay externally-produced raw ASR rows through the same production
/// finalizer, without loading an ASR model. Input rows must join exactly to
/// the frozen candidate manifest by opaque ID, audio SHA-256 and duration.
fn run_finalizer_only(args: &[String]) -> Result<(), String> {
    if args.len() != 4 {
        return Err("usage: ai_off_finalize_replay --finalizer-only <models-dir> <candidate-inputs.jsonl> <asr-rows.jsonl> <output.jsonl>".into());
    }
    isolate_ai_polish_off()?;
    let output_path = PathBuf::from(&args[3]);
    let trace_path = output_path.with_extension("trace.log");
    let trace_file = std::fs::File::create(&trace_path)
        .map_err(|error| format!("create trace log {}: {error}", trace_path.display()))?;
    let _ = tracing_subscriber::fmt()
        .with_target(true)
        .with_ansi(false)
        .with_writer(trace_file)
        .try_init();

    let manifest_path = PathBuf::from(&args[1]);
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("read candidate manifest: {error}"))?;
    let manifest_rows = manifest_text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<InputRow>(line)
                .map_err(|error| format!("parse candidate manifest row: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if manifest_rows.is_empty() {
        return Err("candidate manifest is empty".into());
    }
    let expected = manifest_rows
        .iter()
        .map(|row| (row.id.as_str(), row))
        .collect::<HashMap<_, _>>();
    if expected.len() != manifest_rows.len() {
        return Err("candidate manifest contains duplicate IDs".into());
    }

    let asr_text = std::fs::read_to_string(&args[2])
        .map_err(|error| format!("read external ASR rows: {error}"))?;
    let mut asr_by_id = HashMap::<String, Value>::new();
    for line in asr_text.lines().filter(|line| !line.trim().is_empty()) {
        let value = serde_json::from_str::<Value>(line)
            .map_err(|error| format!("parse external ASR row: {error}"))?;
        if value.get("event").and_then(Value::as_str) == Some("run_config") {
            continue;
        }
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "external ASR row is missing string id".to_string())?
            .to_owned();
        let expected_row = expected
            .get(id.as_str())
            .ok_or_else(|| format!("external ASR row ID {id} is outside candidate manifest"))?;
        let audio_sha256 = value
            .get("audio_sha256")
            .or_else(|| value.get("sha256"))
            .and_then(Value::as_str)
            .ok_or_else(|| format!("external ASR row {id} is missing audio SHA-256"))?;
        if audio_sha256 != expected_row.sha256 {
            return Err(format!("external ASR audio SHA-256 mismatch for {id}"));
        }
        let duration_s = value
            .get("duration_s")
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("external ASR row {id} is missing duration_s"))?;
        if (duration_s - expected_row.duration_s).abs() > 0.001 {
            return Err(format!("external ASR duration mismatch for {id}"));
        }
        if asr_by_id.insert(id.clone(), value).is_some() {
            return Err(format!("duplicate external ASR row ID {id}"));
        }
    }
    if asr_by_id.len() != manifest_rows.len() {
        return Err(format!(
            "external ASR rows do not cover frozen manifest: {} rows for {} IDs",
            asr_by_id.len(),
            manifest_rows.len()
        ));
    }

    let mut engine = EngineRuntime::new(PathBuf::from(&args[0]));
    let finalizer_warmup_started = Instant::now();
    rightkit_dictation_text_pipeline::text_polish::warm();
    let warmup_id = "external-asr-finalizer-warmup";
    engine
        .start_dictation_warmed(warmup_id)
        .map_err(|error| format!("start engine finalizer warmup: {error}"))?;
    engine
        .begin_stop(warmup_id, false, false)
        .map_err(|error| format!("begin engine finalizer warmup stop: {error}"))?;
    let warmup_outcome = engine.finalize_transcript_with_audio_secs(
        warmup_id,
        Ok("Ordinary dictation finalizer warmup.".to_owned()),
        Some(1.0),
    );
    let finalizer_warmup_ms = finalizer_warmup_started.elapsed().as_secs_f64() * 1_000.0;
    if !matches!(
        &warmup_outcome,
        Ok(FinalizeOutcome::Transcript {
            context_request_attempted: Some(false),
            cancelled: false,
            ..
        })
    ) {
        return Err(format!(
            "unscored engine finalizer warmup did not return ordinary Transcript: {warmup_outcome:?}"
        ));
    }

    let mut writer: Box<dyn std::io::Write> = Box::new(
        std::fs::File::create(&output_path)
            .map_err(|error| format!("create output JSONL: {error}"))?,
    );
    writeln_json(
        &mut writer,
        &json!({
            "event": "run_config",
            "mode": "finalizer_only",
            "asr_model_loaded": false,
            "asr_input_source": &args[2],
            "audio_contract": "external candidate rows are joined to frozen manifest id/audio SHA/duration; source WAV bytes are upstream-conditioned 16-kHz mono PCM and are not reloaded or reconditioned",
            "composition_semantics": "upstream pure_asr_total_ms + engine_finalize_ms; excludes capture, audio read and shell delivery/paste",
            "finalizer_warmup_ms": finalizer_warmup_ms,
            "trace_log": trace_path,
            "effective_settings": {
                "asr_backend": "parakeet-tdt",
                "dictation_language": "auto",
                "transcript_cleanup": "clean",
                "ai_polish_enabled": false,
                "ai_polish_mode": "app",
                "is_pro": true,
                "audio_conditioning": false,
                "source": "process-local SettingsBlob override"
            },
            "RIGHTKIT_L3_CLEANUP": false,
            "RIGHTKIT_L3_CLOUD_CONSENT": false,
            "provider_keys": "cleared in process; legacy env variables removed without reading",
            "capture": "excluded",
            "delivery_or_paste": "excluded; stop at FinalizeOutcome::Transcript shell handoff"
        }),
    )?;

    for input in manifest_rows {
        let asr = asr_by_id
            .get(&input.id)
            .ok_or_else(|| format!("validated external ASR row disappeared for {}", input.id))?;
        let pure_asr_text = asr.get("pure_asr_text").and_then(Value::as_str);
        let pure_asr_total_ms = asr.get("pure_asr_total_ms").and_then(Value::as_f64);
        let upstream_status = asr
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let Some(raw_text) = pure_asr_text else {
            writeln_json(
                &mut writer,
                &json!({
                    "event": "replay_row",
                    "id": input.id,
                    "audio_sha256": input.sha256,
                    "duration_s": input.duration_s,
                    "status": "upstream_asr_error",
                    "upstream_status": upstream_status,
                    "upstream_asr": asr,
                    "provider_request_attempted": false
                }),
            )?;
            continue;
        };

        let command_route =
            rightkit_dictation_command_host::command_classify::classify_action(raw_text, true);
        let transform_route =
            rightkit_text_polish::text_pipeline::parse_ai_transform_command(raw_text).is_some()
                || rightkit_text_polish::text_pipeline::is_bare_summarize_trigger(raw_text);
        if command_route.is_some() || transform_route {
            writeln_json(
                &mut writer,
                &json!({
                    "event": "replay_row",
                    "id": input.id,
                    "audio_sha256": input.sha256,
                    "duration_s": input.duration_s,
                    "status": "skipped_nonordinary_route",
                    "reason": if command_route.is_some() { "standalone_command" } else { "ai_transform" },
                    "pure_asr_text": raw_text,
                    "pure_asr_total_ms": pure_asr_total_ms,
                    "upstream_asr": asr,
                    "engine_finalize_ms": null,
                    "asr_plus_finalize_total_ms": null,
                    "provider_request_attempted": false
                }),
            )?;
            continue;
        }

        let session_id = format!("external-asr-{}", input.id);
        engine
            .start_dictation_warmed(&session_id)
            .map_err(|error| format!("start engine replay session: {error}"))?;
        engine
            .begin_stop(&session_id, false, false)
            .map_err(|error| format!("begin engine replay stop: {error}"))?;
        let finalize_started = Instant::now();
        let outcome = engine.finalize_transcript_with_audio_secs(
            &session_id,
            Ok(raw_text.to_owned()),
            Some(input.duration_s as f32),
        );
        let engine_finalize_ms = finalize_started.elapsed().as_secs_f64() * 1_000.0;
        match outcome {
            Ok(FinalizeOutcome::Transcript {
                text,
                raw_text: finalized_raw,
                context_request_attempted: Some(false),
                cancelled: false,
                ..
            }) => {
                let composed_ms = pure_asr_total_ms.map(|value| value + engine_finalize_ms);
                writeln_json(
                    &mut writer,
                    &json!({
                        "event": "replay_row",
                        "id": input.id,
                        "audio_sha256": input.sha256,
                        "duration_s": input.duration_s,
                        "status": "ok",
                        "pure_asr_text": raw_text,
                        "engine_final_text": text,
                        "engine_raw_text": finalized_raw,
                        "pure_asr_total_ms": pure_asr_total_ms,
                        "engine_finalize_ms": engine_finalize_ms,
                        "asr_plus_finalize_total_ms": composed_ms,
                        "upstream_asr": asr,
                        "ai_polish_enabled": false,
                        "cleanup_enabled": false,
                        "cloud_consent": false,
                        "provider_request_attempted": false,
                        "capture_timing": null,
                        "delivery_or_paste_timing": null
                    }),
                )?;
            }
            Ok(other) => writeln_json(
                &mut writer,
                &json!({
                    "event": "replay_row",
                    "id": input.id,
                    "audio_sha256": input.sha256,
                    "duration_s": input.duration_s,
                    "status": "error",
                    "error_stage": "engine_finalize_outcome",
                    "error": format!("unexpected finalize result: {other:?}"),
                    "pure_asr_text": raw_text,
                    "pure_asr_total_ms": pure_asr_total_ms,
                    "engine_finalize_ms": engine_finalize_ms,
                    "upstream_asr": asr,
                    "provider_request_attempted": false
                }),
            )?,
            Err(error) => writeln_json(
                &mut writer,
                &json!({
                    "event": "replay_row",
                    "id": input.id,
                    "audio_sha256": input.sha256,
                    "duration_s": input.duration_s,
                    "status": "error",
                    "error_stage": "engine_finalize",
                    "error": error,
                    "pure_asr_text": raw_text,
                    "pure_asr_total_ms": pure_asr_total_ms,
                    "engine_finalize_ms": engine_finalize_ms,
                    "upstream_asr": asr,
                    "provider_request_attempted": false
                }),
            )?,
        }
    }
    Ok(())
}

fn isolate_ai_polish_off() -> Result<(), String> {
    for name in [
        "GROQ_API_KEY",
        "CEREBRAS_API_KEY",
        "NVIDIA_API_KEY",
        "NIM_API_KEY",
        "OPENROUTER_API_KEY",
    ] {
        std::env::remove_var(name);
    }
    for name in [
        "RIGHTKIT_L3_ROSTER",
        "RIGHTKIT_L3_OPENROUTER_MODEL",
        "RIGHTKIT_L3_OPENROUTER_VALIDATED_FOR",
        "RIGHTKIT_L3_OPENROUTER_BASE_URL",
        "RIGHTKIT_L3_OPENROUTER_PROVIDER_ONLY",
        "RIGHTKIT_L3_OPENROUTER_PROVIDER_ORDER",
        "RIGHTKIT_L3_OPENROUTER_PROVIDER_SORT",
        "RIGHTKIT_L3_GROQ_MODEL",
        "RIGHTKIT_L3_GROQ_BASE_URL",
        "RIGHTKIT_L3_GROQ_FALLBACK_MODEL",
        "RIGHTKIT_L3_GROQ_CLEANUP_MODEL",
        "RIGHTKIT_L3_GROQ_SUMMARY_MODEL",
        "RIGHTKIT_L3_GROQ_CLEANUP_SAME_PROVIDER_FALLBACK_MODEL",
        "RIGHTKIT_L3_GROQ_SUMMARY_SAME_PROVIDER_FALLBACK_MODEL",
        "RIGHTKIT_L3_GROQ_MODEL",
        "RIGHTKIT_L3_GROQ_BASE_URL",
        "RIGHTKIT_L3_NVIDIA_MODEL",
        "RIGHTKIT_L3_NVIDIA_BASE_URL",
        "RIGHTKIT_L3_NVIDIA_CLEANUP_MODEL",
        "RIGHTKIT_L3_NVIDIA_SUMMARY_MODEL",
        "RIGHTKIT_L3_CEREBRAS_MODEL",
        "RIGHTKIT_L3_CEREBRAS_BASE_URL",
        "RIGHTKIT_L3_CEREBRAS_CLEANUP_MODEL",
        "RIGHTKIT_L3_CEREBRAS_SUMMARY_MODEL",
        "RIGHTKIT_L3_PROVIDER_STATE",
        "RIGHTKIT_L3_PROVIDER_TIMEOUT_MS",
        "RIGHTKIT_L3_TOTAL_TIMEOUT_MS",
        "RIGHTKIT_L3_CIRCUIT_COOLDOWN_MS",
        "RIGHTKIT_L3_CIRCUIT_FAILS",
        "RIGHTKIT_L3_MAX_INPUT_CHARS",
        "RIGHTKIT_L3_MIN_WORDS",
        "RIGHTKIT_L3_MIN_AUDIO_SECS",
        "HEARDRIGHT_CLOUD_C1",
        "HEARDRIGHT_CLOUD_C2",
        "HEARDRIGHT_CLOUD_C3",
        "RIGHTKIT_L3_LOG_PAYLOADS",
        "RIGHTKIT_AI_EVAL_CONFIG",
        "RIGHTKIT_AI_EVAL_OUTPUT",
        "RIGHTKIT_BAKEOFF_EXPORT_LIMIT",
        "RIGHTKIT_BAKEOFF_PROMPT_OUT",
        "RIGHTKIT_BAKEOFF_RECENT_CASES",
        "RIGHTKIT_ASR_CONTEXT_BIAS_MAX_TERMS",
        "RIGHTKIT_ASR_CONTEXT_BIAS_SCORE",
        "RIGHTKIT_ASR_COREML",
        "RIGHTKIT_COREML_CU",
        "RIGHTKIT_ASR_EP",
        "RIGHTKIT_ASR_MODEL_SUBDIR",
        "RIGHTKIT_DICTATION_LANG",
        "RIGHTKIT_DML_DEVICE_ID",
        "RIGHTKIT_DISABLE_ASR_CONTEXT_BIAS",
        "RIGHTKIT_RNNT_MAX_TOKENS",
        "RIGHTKIT_TDT_PRUNING",
        "RIGHTKIT_FINAL_TAIL_MS",
        "RIGHTKIT_FINAL_TAIL_NOISE",
        "RIGHTKIT_COREML_DECODER_PROFILE_PROBE",
        "RIGHTKIT_COREML_DURATION_CAP_PROBE",
        "RIGHTKIT_COREML_MAX_SYMBOLS_PROBE",
        "RIGHTKIT_COREML_PRUNING_PROBE",
        "RIGHTKIT_COREML_FUSED_PROBE_DIR",
        "RIGHTKIT_COREML_FUSED_PROBE_PROFILE",
        "RIGHTKIT_COREML_FUSED_PROBE_MAX_ROWS",
        "RIGHTKIT_COREML_INPUT_REUSE_PROBE",
        "RIGHTKIT_COREML_OUTPUT_BACKING_PROBE",
        "RIGHTKIT_COREML_FRONTEND_LOOKAHEAD",
        "RIGHTKIT_COREML_FRONTEND_LOOKAHEAD_MAX_BYTES",
        "RIGHTKIT_COREML_HYP_BATCH",
        "RIGHTKIT_COREML_HYP_BATCH_MAX_ROWS",
        "RIGHTKIT_COREML_ARRAY_ALIGNMENT_PROBE",
        "RIGHTKIT_COREML_ARRAY_PREFETCH_PROBE",
        "RIGHTKIT_COREML_COMPILED_CACHE_DIR",
        "RIGHTKIT_COREML_NO_ROW_FAST_PATH",
        "RIGHTKIT_COREML_DEBUG",
        "RIGHTKIT_COREML_MODEL_DIR",
        "RIGHTKIT_MODELS_DIR",
        "RIGHTKIT_CALIBRATION_COLLECT_DIAGNOSTICS",
        "RIGHTKIT_CALIBRATION_ENCODER_COMPUTE",
        "RIGHTKIT_CALIBRATION_DECODER_COMPUTE",
        "RIGHTKIT_ONBOARDING_CALIBRATION",
        "RIGHTKIT_DISABLE_KWS",
        "RIGHTKIT_TDT_TRACE",
        "RIGHTKIT_ASR_PREDICTOR_DIAGNOSTICS",
        "RIGHTKIT_AUDIO_CONDITIONING_POLICY",
        "RIGHTKIT_AI_EVAL_MODE",
        "RIGHTKIT_ENGINE_TEST_MODE",
        "RIGHTKIT_ASR_DISABLE_PREDICTOR_CACHE",
        "RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT",
        "RIGHTKIT_PREDICTOR_BENCH",
        "RIGHTKIT_ASR_PREDICTOR_DIAGNOSTICS",
        "RIGHTKIT_ASR_WINDOW_STATS_JSON",
        "RIGHTKIT_ASR_BEAM_WIDTH",
        "RIGHTKIT_TDT_DURATION_CAP",
    ] {
        std::env::remove_var(name);
    }
    std::env::set_var("RIGHTKIT_L3_CLEANUP", "0");
    std::env::set_var("RIGHTKIT_L3_CLOUD_CONSENT", "0");
    std::env::set_var("RIGHTKIT_ASR_BACKEND", "parakeet-tdt");
    rightkit_dictation_text_pipeline::provider_keys::clear();

    // Match a Pro user whose AI-polish toggle is off, without consulting or
    // mutating the invoking user's settings. Runtime config is process-local;
    // the separate app-data root below contains only harness diagnostics.
    rightkit_dictation_text_pipeline::settings::replace_runtime_config(
        rightkit_dictation_core::settings::SettingsBlob {
            asr_backend: Some("parakeet-tdt".into()),
            dictation_language: Some("auto".into()),
            transcript_cleanup: Some("clean".into()),
            ai_polish_enabled: Some(false),
            ai_polish_mode: Some("app".into()),
            is_pro: Some(true),
            audio_conditioning: Some(false),
            ..Default::default()
        },
    );

    let root = std::env::temp_dir().join(format!(
        "rightkit-ai-off-replay-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| format!("system time: {error}"))?
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).map_err(|error| format!("create isolated settings: {error}"))?;
    std::env::set_var("RIGHTKIT_APP_DATA_DIR", &root);
    let settings = json!({
        "asr_backend": "parakeet-tdt",
        "dictation_language": "auto",
        "ai_polish_enabled": false,
        "ai_polish_mode": "app",
        "transcript_cleanup": "clean",
        "is_pro": true,
        "audio_conditioning": false,
        "settings_source": "process-local SettingsBlob override; invoking-user preferences are not read"
    });
    std::fs::write(
        root.join("settings.json"),
        serde_json::to_vec_pretty(&settings)
            .map_err(|error| format!("encode isolated settings: {error}"))?,
    )
    .map_err(|error| format!("write isolated settings: {error}"))?;
    Ok(())
}

fn load_wav(path: &Path) -> Result<Vec<f32>, String> {
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

fn error_row(row: InputRow, error: String, stage: &str) -> Value {
    json!({
        "event": "replay_row", "id": row.id, "audio_sha256": row.sha256,
        "duration_s": row.duration_s, "status": "error", "error_stage": stage,
        "error": error, "conditioning_ms": null, "provider_request_attempted": false
    })
}

fn writeln_json(writer: &mut dyn std::io::Write, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *writer, value)
        .map_err(|error| format!("encode output row: {error}"))?;
    writer
        .write_all(b"\n")
        .map_err(|error| format!("write output row: {error}"))
}
