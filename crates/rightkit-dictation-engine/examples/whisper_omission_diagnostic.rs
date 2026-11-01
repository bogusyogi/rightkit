//! Replay the 305 frozen Whisper/KWS recordings through production finalisation.
//!
//! The model path is positional.  Input JSONL contains `id`, `path`,
//! `command`, `onset`, and optionally `speech_start` (or `speech_start_sample`).
//! One output object is written per recording; lifecycle events stay on stdout.

#[cfg(target_os = "macos")]
mod macos {
    use rightkit_dictation_engine::asr::{
        apply_utterance_bias, scheduled_static15_ready_segment, AsrRuntime, ScheduledStatic15,
        AUDIBLE_BLANK_TRANSCRIPTION_ERROR,
    };
    use rightkit_dictation_engine::trigger_audio_cut::{
        resolve_trigger_audio_cut, TriggerAudioCut,
    };
    use rightkit_text_polish::text_pipeline::{
        strip_fired_control_tail, strip_kws_onset_cut_residue, ControlIntent,
    };
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use std::fs::{self, File};
    use std::io::{BufWriter, Write};
    use std::path::Path;
    use std::time::Instant;

    const SAMPLE_RATE: usize = 16_000;
    const EXPECTED_ROWS: usize = 6;

    #[derive(Debug, Deserialize)]
    struct Input {
        id: String,
        path: String,
        command: String,
        onset: usize,
        #[serde(default, alias = "speech_start_sample")]
        speech_start: Option<usize>,
        #[serde(default)]
        floor: Option<usize>,
    }

    #[derive(Debug, Clone, Serialize)]
    struct Arm {
        name: &'static str,
        raw: String,
        clean: String,
        error: Option<String>,
        initial_error: Option<String>,
        elapsed_ms: u128,
        retry_count: usize,
        full_retry: bool,
        cut_source: String,
        cut_at: usize,
        cut_time_ms: f64,
        decode_end: usize,
        tail_samples: usize,
        scheduled_background_windows: usize,
        scheduled_committed_samples: usize,
    }

    #[derive(Debug)]
    struct CachedWindow {
        start: usize,
        end: usize,
        text: String,
    }

    #[derive(Debug)]
    struct Decode {
        result: Result<rightkit_dictation_inference::TranscriptionResult, String>,
        retries: usize,
    }

    fn read_wav(path: &Path) -> Result<Vec<f32>, String> {
        let wav = rightkit_audio::wav::read_wav_mono_f32(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if wav.sample_rate != SAMPLE_RATE as u32 {
            return Err(format!(
                "{}: expected {} Hz WAV, got {} Hz",
                path.display(),
                SAMPLE_RATE as u32,
                wav.sample_rate
            ));
        }
        Ok(wav.samples)
    }

    fn intent(command: &str) -> Result<ControlIntent, String> {
        match command.trim().to_ascii_lowercase().as_str() {
            "zephyr send" | "send" => Ok(ControlIntent::Send),
            "zephyr stop" | "stop" => Ok(ControlIntent::Stop),
            "zephyr cancel" | "cancel" => Ok(ControlIntent::Cancel),
            other => Err(format!("unknown command {other:?}")),
        }
    }

    fn conditioned(raw: &[f32]) -> Vec<f32> {
        let policy = std::env::var("RIGHTKIT_AUDIO_CONDITIONING_POLICY")
            .unwrap_or_else(|_| "default".to_string());
        rightkit_dictation_core::audio_conditioning::condition_for_asr(
            raw,
            SAMPLE_RATE as u32,
            &policy,
        )
    }

    /// Mirrors `transcribe_conditioned(..., BlankPolicy::Final, ...)` from the
    /// worker, including its one retry for an empty final result.
    fn decode_final(
        runtime: &mut AsrRuntime,
        raw: &[f32],
        locked_lang: &mut Option<i32>,
        owner: &'static str,
    ) -> Decode {
        let audio = conditioned(raw);
        lifecycle(
            json!({"event":"decode_input","owner":owner,"raw_samples":raw.len(),"conditioned_samples":audio.len()}),
        );
        let mut retries = 0;
        let mut result = runtime.with_inference_lease(owner, |model| {
            apply_utterance_bias(model);
            model.transcribe_final_result_under_lease_locked(&audio, locked_lang)
        });
        if matches!(&result, Ok(value) if value.text.trim().is_empty())
            || matches!(&result, Err(error) if error == AUDIBLE_BLANK_TRANSCRIPTION_ERROR)
        {
            retries = 1;
            result = runtime.with_inference_lease(owner, |model| {
                apply_utterance_bias(model);
                model.transcribe_final_result_under_lease_locked(&audio, locked_lang)
            });
        }
        if matches!(&result, Ok(value) if value.text.trim().is_empty()) {
            result = Err(AUDIBLE_BLANK_TRANSCRIPTION_ERROR.to_string());
        }
        lifecycle(
            json!({"event":"decode_output","owner":owner,"text":result.as_ref().ok().map(|x| &x.text),"error":result.as_ref().err(),"locked_lang":locked_lang}),
        );
        Decode { result, retries }
    }

    fn build_background(
        runtime: &mut AsrRuntime,
        raw: &[f32],
        locked_lang: &mut Option<i32>,
    ) -> (Vec<CachedWindow>, Option<String>) {
        let mut scheduler = ScheduledStatic15::default();
        let mut windows = Vec::new();
        while let Some(segment) =
            scheduled_static15_ready_segment(raw, scheduler.committed_samples())
        {
            let start = segment.start;
            let end = segment.end;
            let decoded = decode_final(runtime, &raw[segment], locked_lang, "kws_background");
            let text = match decoded.result {
                Ok(result) => result.text,
                Err(error) => return (windows, Some(error)),
            };
            if let Err(error) = scheduler.commit_window(start, end, &text, Vec::new()) {
                return (windows, Some(error));
            }
            lifecycle(json!({"event":"background_window","start":start,"end":end,"text":text}));
            windows.push(CachedWindow { start, end, text });
        }
        (windows, None)
    }

    fn scheduler_from_cache(windows: &[CachedWindow]) -> Result<ScheduledStatic15, String> {
        let mut scheduler = ScheduledStatic15::default();
        for window in windows {
            scheduler.commit_window(window.start, window.end, &window.text, Vec::new())?;
        }
        Ok(scheduler)
    }

    #[allow(clippy::too_many_arguments)] // diagnostic example: one flat call per arm
    fn finish_arm(
        runtime: &mut AsrRuntime,
        raw: &[f32],
        decode_end: usize,
        cut_source: String,
        name: &'static str,
        onset_cut: bool,
        command: ControlIntent,
        windows: &[CachedWindow],
        _background_error: Option<&String>,
        locked_lang: &mut Option<i32>,
    ) -> Arm {
        let started = Instant::now();
        let conditioned_end = decode_end.min(raw.len());
        let mut scheduler = match scheduler_from_cache(windows) {
            Ok(value) => value,
            Err(error) => {
                return Arm {
                    name,
                    raw: String::new(),
                    clean: String::new(),
                    error: Some(error),
                    initial_error: None,
                    elapsed_ms: started.elapsed().as_millis(),
                    retry_count: 0,
                    full_retry: false,
                    cut_time_ms: conditioned_end as f64 * 1_000.0 / SAMPLE_RATE as f64,
                    cut_at: conditioned_end,
                    decode_end: conditioned_end,
                    tail_samples: 0,
                    cut_source,
                    scheduled_background_windows: windows.len(),
                    scheduled_committed_samples: 0,
                };
            }
        };
        let mut retry_count = 0;
        let mut tail_samples = 0;
        let mut first_error = None;
        let mut result = if conditioned_end == 0 {
            Ok(rightkit_dictation_inference::TranscriptionResult {
                text: String::new(),
                tokens: Vec::new(),
            })
        } else {
            scheduler.finish_result(&raw[..conditioned_end], |tail| {
                tail_samples = tail.len();
                let decoded = decode_final(runtime, tail, locked_lang, "kws_final_tail");
                retry_count += decoded.retries;
                decoded.result
            })
        };
        let mut full_retry = false;
        let mut actual_onset_cut = onset_cut;
        let mut actual_decode_end = conditioned_end;
        if result.is_err() && conditioned_end < raw.len() {
            first_error = result.as_ref().err().cloned();
            full_retry = true;
            actual_onset_cut = false;
            actual_decode_end = raw.len();
            result = scheduler.finish_result(raw, |tail| {
                tail_samples = tail.len();
                let decoded = decode_final(runtime, tail, locked_lang, "kws_full_retry");
                retry_count += decoded.retries;
                decoded.result
            });
        }
        let (raw_text, clean, error) = match result {
            Ok(value) => {
                let residue = if actual_onset_cut {
                    strip_kws_onset_cut_residue(&value.text)
                } else {
                    value.text.clone()
                };
                (
                    value.text,
                    strip_fired_control_tail(&residue, command),
                    None,
                )
            }
            Err(error) => (String::new(), String::new(), Some(error)),
        };
        let committed = scheduler.committed_samples();
        Arm {
            name,
            raw: raw_text,
            clean,
            error,
            initial_error: first_error,
            elapsed_ms: started.elapsed().as_millis(),
            retry_count,
            full_retry,
            cut_source,
            cut_at: conditioned_end,
            cut_time_ms: conditioned_end as f64 * 1_000.0 / SAMPLE_RATE as f64,
            decode_end: actual_decode_end,
            tail_samples,
            scheduled_background_windows: scheduler.background_windows(),
            scheduled_committed_samples: committed,
        }
    }

    fn lifecycle(value: serde_json::Value) {
        println!("{}", value);
        let _ = std::io::stdout().flush();
    }

    pub fn run() -> Result<(), String> {
        std::env::set_var("RIGHTKIT_WHISPER_DUMP", "1");
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.len() != 3 {
            return Err("usage: whisper_kws_cleanup_replay MODEL INPUTS_JSONL OUTPUT_JSONL".into());
        }
        let rows: Vec<Input> = fs::read_to_string(&args[1])
            .map_err(|e| e.to_string())?
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        if rows.len() != EXPECTED_ROWS {
            return Err(format!(
                "expected {EXPECTED_ROWS} inputs, got {}",
                rows.len()
            ));
        }

        // Force the production Whisper CoreML variant.  `dictation_language`
        // is auto so each recording uses the runtime's resolved language lock.
        std::env::set_var("RIGHTKIT_ASR_BACKEND", "whisper");
        std::env::set_var("RIGHTKIT_ASR_COREML", "1");
        std::env::set_var("RIGHTKIT_DICTATION_LANG", "auto");
        std::env::set_var("RIGHTKIT_WHISPER_COREML_MODEL", &args[0]);
        if Path::new(&args[2]).exists() {
            return Err(format!("output already exists: {}", args[2]));
        }
        let model_path = Path::new(&args[0]);
        lifecycle(json!({
            "event": "start",
            "clips": rows.len(),
            "runtime": "WhisperCoreMl",
            "scheduled_static15": true,
        }));
        let engine = rightkit_dictation_inference::whisper_coreml::WhisperCoreMl::load(model_path)?;
        let lang_tok = engine.lang_token("auto");
        let mut runtime = AsrRuntime::WhisperCoreMl { engine, lang_tok };
        if !matches!(&runtime, AsrRuntime::WhisperCoreMl { .. }) {
            return Err("runtime loader did not return WhisperCoreMl".into());
        }
        if !runtime.uses_scheduled_static15() {
            return Err("Whisper runtime did not select production static-15 scheduling".into());
        }
        lifecycle(json!({"event": "model_loaded", "runtime": "WhisperCoreMl"}));

        let mut output = BufWriter::new(File::create(&args[2]).map_err(|e| e.to_string())?);
        let mut systemic_failures = 0usize;
        for (index, row) in rows.iter().enumerate() {
            lifecycle(json!({"event":"clip_start","id":row.id}));
            let raw = read_wav(Path::new(&row.path))?;
            lifecycle(json!({"event":"audio","samples":raw.len(),"onset":row.onset}));
            let command = intent(&row.command)?;
            let speech_start = row.speech_start.or(row.floor);
            let resolved = resolve_trigger_audio_cut(Some(row.onset), None, speech_start, &raw);
            let (resolved_end, resolved_source) = match resolved {
                TriggerAudioCut::Cut {
                    cut_at, cut_source, ..
                } => (cut_at, cut_source.to_string()),
                TriggerAudioCut::FullBuffer { reason } => {
                    (raw.len(), format!("full_buffer:{reason}"))
                }
            };
            let (mut locked_lang, background) = (None, Instant::now());
            let (windows, background_error) =
                build_background(&mut runtime, &raw, &mut locked_lang);
            let background_ms = background.elapsed().as_millis();
            let background_lang = locked_lang;

            // Cancel keeps production's full-history path.  Other controls use
            // the resolved KWS cut, with the onset residue strip only on an
            // actual onset cut.  Comparator always decodes full audio.
            let cancel_full = matches!(command, ControlIntent::Cancel);
            let current_end = if cancel_full { raw.len() } else { resolved_end };
            let current_source = if cancel_full {
                "cancel_full_history".to_string()
            } else {
                resolved_source.clone()
            };
            let current_onset =
                !cancel_full && resolved_source == "kws_onset" && current_end < raw.len();
            let mut current_lang = background_lang;
            let current = finish_arm(
                &mut runtime,
                &raw,
                current_end,
                current_source,
                "current_cut",
                current_onset,
                command,
                &windows,
                background_error.as_ref(),
                &mut current_lang,
            );
            let mut full_lang = background_lang;
            let full = finish_arm(
                &mut runtime,
                &raw,
                raw.len(),
                "full_audio".to_string(),
                "full_text_cleanup",
                false,
                command,
                &windows,
                background_error.as_ref(),
                &mut full_lang,
            );
            let systemic_error = current.error.is_some() && full.error.is_some();
            let tail_start = raw.len().saturating_sub(15 * SAMPLE_RATE);
            lifecycle(json!({"event":"independent_tail","start":tail_start,"end":raw.len()}));
            let mut probe_lang = background_lang;
            let probe = decode_final(
                &mut runtime,
                &raw[tail_start..],
                &mut probe_lang,
                "independent_last15",
            );
            std::env::set_var("RIGHTKIT_WHISPER_NOTS", "1");
            let mut nots_lang = background_lang;
            let nots = decode_final(
                &mut runtime,
                &raw[tail_start..],
                &mut nots_lang,
                "independent_last15_no_timestamps",
            );
            std::env::remove_var("RIGHTKIT_WHISPER_NOTS");
            let row_output = json!({
                "independent_tail":probe.result.map(|x|x.text),
                "independent_tail_no_timestamps":nots.result.map(|x|x.text),
                "id": row.id,
                "command": row.command,
                "onset": row.onset,
                "speech_start": speech_start,
                "background_ms": background_ms,
                "background_error": background_error,
                "arms": [current, full],
            });
            serde_json::to_writer(&mut output, &row_output).map_err(|e| e.to_string())?;
            writeln!(output).map_err(|e| e.to_string())?;
            output.flush().map_err(|e| e.to_string())?;
            lifecycle(json!({
                "event": "clip_complete",
                "completed": index + 1,
                "total": rows.len(),
                "id": row.id,
            }));
            if systemic_error {
                systemic_failures += 1;
                if systemic_failures >= 3 {
                    lifecycle(json!({
                        "event": "systemic_failure",
                        "completed": index + 1,
                        "consecutive": systemic_failures,
                    }));
                    return Err("three consecutive recordings failed in both arms".into());
                }
            } else {
                systemic_failures = 0;
            }
        }
        lifecycle(json!({"event": "complete", "clips": rows.len()}));
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), String> {
    macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("whisper_kws_cleanup_replay is macOS-only");
}
