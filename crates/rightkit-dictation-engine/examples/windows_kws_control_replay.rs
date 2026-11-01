//! Native Windows replay of production KWS confirmation & control cleanup.
//! Input JSON: {"rows":[{"id":"...","path":"...","expected":"zephyr send"}]}
//! Run through RightKit: cargo run --release --example windows_kws_control_replay -- INPUT.json MODELS_DIR [--kws-only|--asr-only] [--feed-20ms] [--backend sherpa|rust]
//! `--backend` picks the KWS runtime (default: `RIGHTKIT_KWS_BACKEND` in test-mode builds, else
//! Sherpa); every output line carries `"backend"`, so two runs diff row by row.

#[cfg(target_os = "windows")]
fn main() -> Result<(), String> {
    use rightkit_dictation_engine::asr::{AsrEp, AsrRuntime};
    use rightkit_dictation_engine::sherpa_kws::{KwsBackend, SherpaKws};
    use rightkit_dictation_inference::control_cleanup::cleanup_confirmed_control_with_exact_retry;
    use rightkit_text_polish::text_pipeline::ControlIntent;
    use serde::Deserialize;
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    #[derive(Deserialize)]
    struct Row {
        id: String,
        path: PathBuf,
        expected: Option<String>,
        #[serde(default)]
        asr: bool,
    }
    #[derive(Deserialize)]
    struct Manifest {
        rows: Vec<Row>,
    }
    fn wav(path: &Path) -> Result<Vec<f32>, String> {
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
    let mut args = std::env::args().skip(1);
    let input = args.next().ok_or("usage: INPUT.json MODELS_DIR")?;
    let models_dir = PathBuf::from(args.next().ok_or("usage: INPUT.json MODELS_DIR")?);
    let flags: Vec<String> = args.collect();
    let backend = match flags
        .iter()
        .position(|flag| flag == "--backend")
        .map(|index| flags.get(index + 1).map(String::as_str))
    {
        None => KwsBackend::from_env(),
        Some(Some("sherpa")) => KwsBackend::Sherpa,
        Some(Some("rust")) => KwsBackend::Rust,
        Some(_) => return Err("--backend must be sherpa or rust".into()),
    };
    let backend_label = match backend {
        KwsBackend::Sherpa => "sherpa",
        KwsBackend::Rust => "rust",
    };
    let kws_only = flags.iter().any(|flag| flag == "--kws-only");
    let asr_only = flags.iter().any(|flag| flag == "--asr-only");
    // Diagnostic comparison: use exact alignment through production cleanup's
    // callback to distinguish locator boundaries from text/token trimming.
    let force_exact_cleanup = flags.iter().any(|flag| flag == "--force-exact-cleanup");
    let feed_samples = if flags.iter().any(|flag| flag == "--feed-20ms") {
        320
    } else {
        800
    };
    let manifest_bytes = std::fs::read(&input).map_err(|e| e.to_string())?;
    let manifest: Manifest = match serde_json::from_slice(&manifest_bytes) {
        Ok(manifest) => manifest,
        Err(_) if asr_only => Manifest {
            rows: String::from_utf8_lossy(&manifest_bytes)
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(serde_json::from_str::<serde_json::Value>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|value| Row {
                    id: value["id"].as_str().unwrap_or_default().to_string(),
                    path: PathBuf::from(value["path"].as_str().unwrap_or_default()),
                    expected: value["expected"].as_str().map(str::to_owned),
                    asr: true,
                })
                .collect(),
        },
        Err(error) => return Err(error.to_string()),
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources");
    rightkit_onnx::configure_runtime_with_options(
        [rightkit_onnx::RuntimeCandidate::new(
            root.join("runtime/onnxruntime.dll"),
            rightkit_onnx::CandidateSource::Explicit,
        )],
        &rightkit_onnx::RuntimeBindingOptions {
            force_replace: true,
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    if asr_only {
        std::env::set_var("RIGHTKIT_ASR_BACKEND", "parakeet-tdt");
        let mut asr = AsrRuntime::load(&models_dir, AsrEp::Dml)?;
        for row in manifest.rows {
            let audio = wav(&row.path)?;
            let conditioned = rightkit_dictation_core::audio_conditioning::condition_for_asr(
                &audio, 16_000, "default",
            );
            let began = Instant::now();
            let result = asr.transcribe_final_result_under_lease(&conditioned)?;
            let decode_ms = began.elapsed().as_millis();
            let raw_tokens = result
                .tokens
                .iter()
                .map(|token| {
                    json!({
                        "text": token.text,
                        "start": token.start,
                        "end": token.end,
                    })
                })
                .collect::<Vec<_>>();
            println!(
                "{}",
                json!({
                    "id": row.id,
                    "audio_samples": audio.len(),
                    "raw": result.text,
                    "raw_tokens": raw_tokens,
                    "decode_ms": decode_ms,
                })
            );
        }
        return Ok(());
    }
    // ORT is bound above, so the Rust backend's sessions and Sherpa's preload share one runtime.
    let mut kws = SherpaKws::load_with_backend(&root.join("kws"), backend)?;
    let mut asr = if kws_only {
        None
    } else {
        std::env::set_var("RIGHTKIT_ASR_BACKEND", "parakeet-tdt");
        Some(AsrRuntime::load(&models_dir, AsrEp::Dml)?)
    };
    println!(
        "{}",
        json!({"event":"loaded","backend":backend_label,"cleanup_supported":asr.as_ref().is_some_and(|runtime| runtime.supports_control_cleanup()),"kws_only":kws_only})
    );
    let mut fresh_attempts = 0usize;
    let mut fallback_attempts = 0usize;
    for row in manifest.rows {
        let audio = wav(&row.path)?;
        kws.reset_stream()?;
        let began = Instant::now();
        let mut armed = false;
        let mut streamed = String::new();
        let mut onset = None;
        let mut steps = 0usize;
        for live in (1_600..audio.len())
            .step_by(1_600)
            .chain(std::iter::once(audio.len()))
        {
            let start = live.saturating_sub(3 * 16_000);
            let (result, wake_start) = kws.transcribe_cascade_result_with_wake_start(
                &audio[start..live],
                start,
                live,
                armed,
            )?;
            steps += 1;
            if result.text == "zephyr" {
                armed = true;
            }
            if matches!(
                result.text.as_str(),
                "zephyr stop" | "zephyr send" | "zephyr cancel"
            ) {
                streamed = result.text;
                onset = wake_start;
                break;
            }
        }
        let stream_ms = began.elapsed().as_millis();
        // Reproduce worker scheduling: full-tail fallback is submitted only
        // after a bare wake has armed pending_prefix and the production 50 ms
        // full-tail cadence has elapsed.
        // Fresh confirmation owns that snapshot; an empty fresh result falls
        // back to cascade on the exact same snapshot.
        kws.reset_stream()?;
        let fresh_began = Instant::now();
        let mut fresh_text = String::new();
        let mut fresh_onset = None;
        let mut fresh_invoked = false;
        let mut fresh_result_empty = None;
        let mut fallback_invoked = false;
        let mut fallback_text = String::new();
        let mut fallback_onset = None;
        let mut full_lane_text = String::new();
        let mut full_lane_onset = None;
        let mut fresh_attempts_row = 0usize;
        let mut fallback_attempts_row = 0usize;
        let mut pending_prefix = false;
        let mut pending_since = None;
        let mut last_full_tail_probe_samples = 0usize;
        let mut fresh_snapshot_samples = None;
        for live in (feed_samples..audio.len())
            .step_by(feed_samples)
            .chain(std::iter::once(audio.len()))
        {
            if pending_since.is_some_and(|since| {
                rightkit_dictation_runtime::worker::replay_policy::tail_prefix_expired_samples(
                    live.saturating_sub(since),
                )
            }) {
                pending_prefix = false;
                pending_since = None;
            }
            if !rightkit_dictation_runtime::worker::replay_policy::tail_probe_due(live, true) {
                continue;
            }
            let start = live.saturating_sub(3 * 16_000);
            let snapshot = &audio[start..live];
            let full_due = pending_prefix
                && rightkit_dictation_runtime::worker::replay_policy::full_tail_fallback_due(
                    pending_prefix,
                    live.saturating_sub(last_full_tail_probe_samples),
                );
            let result = if full_due {
                last_full_tail_probe_samples = live;
                fresh_invoked = true;
                fresh_attempts_row += 1;
                fresh_snapshot_samples = Some(snapshot.len());
                let (scheduled, wake_start, fresh_used) = kws
                    .transcribe_scheduled_probe_result_with_wake_start(
                        snapshot,
                        start,
                        live,
                        pending_prefix,
                        true,
                    )?;
                fresh_result_empty = Some(!fresh_used);
                fresh_text = scheduled.text;
                fresh_onset = wake_start;
                full_lane_text = fresh_text.clone();
                full_lane_onset = fresh_onset;
                fallback_invoked = !fresh_used;
                if !fresh_used {
                    fallback_attempts_row += 1;
                    fallback_text = fresh_text.clone();
                    fallback_onset = fresh_onset;
                }
                fresh_text.clone()
            } else {
                let (stream, wake_start) = kws.transcribe_cascade_result_with_wake_start(
                    snapshot,
                    start,
                    live,
                    pending_prefix,
                )?;
                full_lane_text = stream.text.clone();
                full_lane_onset = wake_start;
                stream.text
            };
            if matches!(
                result.as_str(),
                "zephyr stop" | "zephyr send" | "zephyr cancel"
            ) {
                break;
            }
            pending_prefix =
                rightkit_dictation_runtime::worker::replay_policy::candidate_armed_after_result(
                    &result,
                );
            if pending_prefix && pending_since.is_none() {
                pending_prefix = true;
                pending_since = Some(live);
            } else if !pending_prefix {
                pending_since = None;
            }
        }
        let fresh_ms = fresh_began.elapsed().as_millis();
        let mut cleanup = None;
        let mut cleanup_elapsed_ms = None;
        let final_text = full_lane_text.as_str();
        let final_onset = full_lane_onset;
        fresh_attempts += fresh_attempts_row;
        fallback_attempts += fallback_attempts_row;
        if let (Some(asr), true, Some(onset), Some(intent)) = (
            asr.as_mut(),
            row.asr && !kws_only,
            final_onset,
            match final_text {
                "zephyr stop" => Some(ControlIntent::Stop),
                "zephyr send" => Some(ControlIntent::Send),
                _ => None,
            },
        ) {
            let conditioned = rightkit_dictation_core::audio_conditioning::condition_for_asr(
                &audio, 16_000, "default",
            );
            let asr_began = Instant::now();
            let full = asr.transcribe_final_result_under_lease(&conditioned)?;
            let decode_ms = asr_began.elapsed().as_millis();
            let cleaned = cleanup_confirmed_control_with_exact_retry(
                full.clone(),
                &audio,
                intent,
                onset,
                |conditioned, command, earliest, exact| {
                    if exact || force_exact_cleanup {
                        asr.align_control_suffix_exact_under_lease(conditioned, command, earliest)
                    } else {
                        asr.align_control_suffix_under_lease(conditioned, command, earliest)
                    }
                },
            );
            let token_json = |tokens: &[rightkit_dictation_inference::TimedToken]| {
                tokens
                    .iter()
                    .map(|token| {
                        json!({
                            "text": token.text,
                            "start": token.start,
                            "end": token.end,
                        })
                    })
                    .collect::<Vec<_>>()
            };
            cleanup_elapsed_ms = Some(asr_began.elapsed().as_millis());
            cleanup = Some(json!({
                "raw":full.text,"clean":cleaned.transcript.text,
                "raw_tokens":token_json(&full.tokens),
                "clean_tokens":token_json(&cleaned.transcript.tokens),
                "method":cleaned.method,"boundary_sec":cleaned.boundary_sec,
                "score":cleaned.score,"joint_calls":cleaned.joint_calls,
                "error":cleaned.error,"decode_ms":decode_ms,
            }));
        }
        println!(
            "{}",
            json!({
                "id":row.id,"backend":backend_label,"expected":row.expected,"streamed":streamed,
                "onset_sample":onset,"stream_ms":stream_ms,"steps":steps,
                "fresh":fresh_text,"fresh_onset_sample":fresh_onset,"fresh_ms":fresh_ms,
                "fresh_invoked":fresh_invoked,"fresh_result_empty":fresh_result_empty,
                "fresh_snapshot_samples":fresh_snapshot_samples,
                "fresh_tail_samples":3_200,"stream_padding_samples":0,
                "stream_feed_samples":1_600,"full_feed_samples":feed_samples,
                "fallback_invoked":fallback_invoked,"fallback":fallback_text,
                "fallback_onset_sample":fallback_onset,
                "full_lane":full_lane_text,"full_lane_onset_sample":full_lane_onset,
                "stream_wake_onset_sample":onset,
                "full_wake_onset_sample":full_lane_onset,
                "stream_confirmation_sample":null,
                "full_confirmation_sample":null,
                "fresh_attempts":fresh_attempts_row,
                "fallback_attempts":fallback_attempts_row,
                "final":full_lane_text,"final_onset_sample":full_lane_onset,
                "audio_samples":audio.len(),"cleanup":cleanup,
                "cleanup_elapsed_ms":cleanup_elapsed_ms,
            })
        );
    }
    println!(
        "{}",
        json!({
            "event":"replay_summary",
            "backend":backend_label,
            "fresh_attempts":fresh_attempts,
            "fallback_attempts":fallback_attempts,
        })
    );
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("windows_kws_control_replay requires Windows");
}
