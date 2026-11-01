#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("static15_l1_chunk_smoke is macOS-only");
}

#[cfg(target_os = "macos")]
mod mac {
    use rightkit_dictation_engine::asr::{
        append_scheduled_static15_text, scheduled_static15_ready_segment, AsrEp, AsrRuntime,
    };
    use rightkit_dictation_engine::l3_cleanup::{
        app_polish_outcome, CleanupOutcome, PolishContext,
    };
    use serde::{Deserialize, Serialize};
    use std::fs::File;
    use std::io::{BufWriter, Write};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    const SAMPLE_RATE: usize = 16_000;
    const EXPECTED_CLIPS: usize = 3;
    const MAX_PROVIDER_REQUESTS: usize = 13;
    const PROVIDER_PACING_MS: u64 = 15_000;
    const GROQ_PRIMARY: &str = "qwen/qwen3.6-27b";

    #[derive(Clone, Deserialize)]
    struct ManifestRow {
        id: String,
        duration_s: f64,
    }

    struct DecodedClip {
        row: ManifestRow,
        chunks: Vec<DecodedChunk>,
        transcript: String,
        total_asr_ms: f64,
        stop_asr_ms: f64,
    }

    struct DecodedChunk {
        kind: &'static str,
        start_s: f64,
        end_s: f64,
        eligible_at_s: f64,
        asr_ms: f64,
        asr_finished_at_s: f64,
        text: String,
    }

    #[derive(Serialize)]
    struct PolishResult {
        status: &'static str,
        reason: Option<&'static str>,
        elapsed_ms: f64,
        text: String,
    }

    #[derive(Serialize)]
    struct ChunkResult {
        kind: &'static str,
        start_s: f64,
        end_s: f64,
        eligible_at_s: f64,
        asr_ms: f64,
        asr_finished_at_s: f64,
        raw_text: String,
        local_text: String,
        polish: PolishResult,
        polish_started_at_s: f64,
        polish_finished_at_s: f64,
    }

    #[derive(Serialize)]
    struct OutputRow {
        schema: &'static str,
        provider: &'static str,
        model: &'static str,
        chunk_context: &'static str,
        id: String,
        duration_s: f64,
        background_windows: usize,
        tail_audio_s: f64,
        raw_scheduled_text: String,
        local_full_text: String,
        total_asr_ms: f64,
        stop_asr_ms: f64,
        provider_warmup_ms: f64,
        full_final: PolishResult,
        full_final_stop_ms: f64,
        chunk_results: Vec<ChunkResult>,
        chunk_repair_text: String,
        chunk_total_l1_ms: f64,
        chunk_background_l1_ms: f64,
        chunk_tail_l1_ms: f64,
        chunk_background_missed_deadline: bool,
        chunk_repair_stop_ms: f64,
        stop_latency_delta_ms: f64,
        output_identical: bool,
    }

    pub fn run() -> Result<(), String> {
        let args: Vec<String> = std::env::args().collect();
        if args.len() != 8 {
            return Err(
                "usage: static15_l1_chunk_smoke <models-root> <manifest> <audio-dir> <output.jsonl> <clip-1> <clip-2> <clip-3>".into(),
            );
        }
        require_nonempty_env("GROQ_API_KEY")?;
        configure_qwen_only(&args[4])?;

        let models_root = PathBuf::from(&args[1]);
        let manifest_path = PathBuf::from(&args[2]);
        let audio_dir = PathBuf::from(&args[3]);
        let output_path = PathBuf::from(&args[4]);
        let requested = &args[5..];
        if requested.len() != EXPECTED_CLIPS {
            return Err(format!("exactly {EXPECTED_CLIPS} clip ids are required"));
        }

        let manifest: Vec<ManifestRow> = serde_json::from_str(
            &std::fs::read_to_string(&manifest_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let rows = requested
            .iter()
            .map(|id| {
                let row = manifest
                    .iter()
                    .find(|row| row.id == *id)
                    .cloned()
                    .ok_or_else(|| format!("clip not present in canonical manifest: {id}"))?;
                if row.duration_s <= 15.0 {
                    return Err(format!("clip is not over 15 seconds: {id}"));
                }
                Ok(row)
            })
            .collect::<Result<Vec<_>, String>>()?;

        std::env::set_var("RIGHTKIT_ASR_BACKEND", "parakeet-tdt");
        let mut model = AsrRuntime::load(&models_root, AsrEp::resolve_default())?;
        let first_audio = load_wav(&audio_dir.join(format!("{}.wav", rows[0].id)))?;
        let _ = model.transcribe_result(&first_audio[..SAMPLE_RATE.min(first_audio.len())])?;

        let mut decoded = Vec::with_capacity(rows.len());
        for row in rows {
            let audio = load_wav(&audio_dir.join(format!("{}.wav", row.id)))?;
            decoded.push(decode_exact_static15(&mut model, row, &audio)?);
        }

        let provider_requests = 1 + decoded
            .iter()
            .map(|clip| 1 + clip.chunks.len())
            .sum::<usize>();
        if provider_requests > MAX_PROVIDER_REQUESTS {
            return Err(format!(
                "provider request cap exceeded: {provider_requests} > {MAX_PROVIDER_REQUESTS}"
            ));
        }

        let context = PolishContext {
            app_name: Some("ChatGPT".to_string()),
            window_title: Some("ChatGPT".to_string()),
            field_context_available: false,
            writing_region: Some("India".to_string()),
            ..Default::default()
        };
        let warm_input = "Please send the updated project invoice tomorrow morning after reviewing the final totals.";
        let warm_local = rightkit_dictation_engine::text_polish::polish_local_only(warm_input);
        let warm = polish_once(&warm_local, &context);
        if warm.status != "cleaned" {
            return Err(format!(
                "Groq Qwen warmup failed: {}",
                warm.reason.unwrap_or(warm.status)
            ));
        }

        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let output_temp = output_path.with_extension("jsonl.tmp");
        let mut writer =
            BufWriter::new(File::create(&output_temp).map_err(|error| error.to_string())?);
        println!(
            "qwen warmup={:.1}ms requests={} clips={}",
            warm.elapsed_ms,
            provider_requests,
            decoded.len()
        );

        for (index, clip) in decoded.into_iter().enumerate() {
            let local_full =
                rightkit_dictation_engine::text_polish::polish_local_only(&clip.transcript);
            pace_provider();
            let full_final = polish_once(&local_full, &context);
            let full_final_stop_ms = clip.stop_asr_ms + full_final.elapsed_ms;

            let mut chunk_results = Vec::with_capacity(clip.chunks.len());
            let mut chunk_repair_text = String::new();
            let mut l1_available_at_s = 0.0f64;
            let mut chunk_total_l1_ms = 0.0f64;
            let mut chunk_background_l1_ms = 0.0f64;
            let mut chunk_background_missed_deadline = false;

            let chunks = clip.chunks;
            let tail_index = chunks.len().saturating_sub(1);
            let suffix_start = tail_index.saturating_sub(1);
            for (chunk_index, chunk) in chunks[..tail_index].iter().enumerate() {
                let local = rightkit_dictation_engine::text_polish::polish_local_only_with(
                    &chunk.text,
                    chunk_index == 0,
                );
                pace_provider();
                let polish = polish_once(&local, &context);
                let polish_started_at_s = chunk.asr_finished_at_s.max(l1_available_at_s);
                let polish_finished_at_s = polish_started_at_s + polish.elapsed_ms / 1_000.0;
                l1_available_at_s = polish_finished_at_s;
                chunk_total_l1_ms += polish.elapsed_ms;
                chunk_background_l1_ms += polish.elapsed_ms;
                if polish_finished_at_s > clip.row.duration_s {
                    chunk_background_missed_deadline = true;
                }
                if chunk_index < suffix_start {
                    append_scheduled_static15_text(&mut chunk_repair_text, &polish.text);
                }
                chunk_results.push(ChunkResult {
                    kind: chunk.kind,
                    start_s: chunk.start_s,
                    end_s: chunk.end_s,
                    eligible_at_s: chunk.eligible_at_s,
                    asr_ms: chunk.asr_ms,
                    asr_finished_at_s: chunk.asr_finished_at_s,
                    raw_text: chunk.text.clone(),
                    local_text: local,
                    polish,
                    polish_started_at_s,
                    polish_finished_at_s,
                });
            }

            let mut suffix_raw = String::new();
            for chunk in &chunks[suffix_start..] {
                append_scheduled_static15_text(&mut suffix_raw, &chunk.text);
            }
            let suffix_local = rightkit_dictation_engine::text_polish::polish_local_only_with(
                &suffix_raw,
                suffix_start == 0,
            );
            pace_provider();
            let suffix_polish = polish_once(&suffix_local, &context);
            let tail = chunks.last().expect("decoded clip always has a tail");
            let suffix_started_at_s = tail.asr_finished_at_s.max(l1_available_at_s);
            let suffix_finished_at_s = suffix_started_at_s + suffix_polish.elapsed_ms / 1_000.0;
            l1_available_at_s = suffix_finished_at_s;
            chunk_total_l1_ms += suffix_polish.elapsed_ms;
            let chunk_tail_l1_ms = suffix_polish.elapsed_ms;
            append_scheduled_static15_text(&mut chunk_repair_text, &suffix_polish.text);
            chunk_results.push(ChunkResult {
                kind: "tail_with_one_chunk_lookbehind",
                start_s: chunks[suffix_start].start_s,
                end_s: tail.end_s,
                eligible_at_s: tail.eligible_at_s,
                asr_ms: tail.asr_ms,
                asr_finished_at_s: tail.asr_finished_at_s,
                raw_text: suffix_raw,
                local_text: suffix_local,
                polish: suffix_polish,
                polish_started_at_s: suffix_started_at_s,
                polish_finished_at_s: suffix_finished_at_s,
            });

            let chunk_repair_text = chunk_repair_text.trim().to_string();
            let chunk_repair_stop_ms =
                ((l1_available_at_s - clip.row.duration_s).max(0.0)) * 1_000.0;
            let stop_latency_delta_ms = chunk_repair_stop_ms - full_final_stop_ms;
            let output_identical = full_final.text == chunk_repair_text;
            let background_windows = chunk_results
                .iter()
                .filter(|chunk| chunk.kind == "background")
                .count();
            let tail_audio_s = chunk_results
                .last()
                .map(|chunk| chunk.end_s - chunk.start_s)
                .unwrap_or_default();

            let row = OutputRow {
                schema: "heardright.static15_l1_chunk_smoke.v1",
                provider: "groq",
                model: GROQ_PRIMARY,
                chunk_context: "one_raw_chunk_lookbehind",
                id: clip.row.id,
                duration_s: clip.row.duration_s,
                background_windows,
                tail_audio_s,
                raw_scheduled_text: clip.transcript,
                local_full_text: local_full,
                total_asr_ms: clip.total_asr_ms,
                stop_asr_ms: clip.stop_asr_ms,
                provider_warmup_ms: warm.elapsed_ms,
                full_final_stop_ms,
                full_final,
                chunk_results,
                chunk_repair_text,
                chunk_total_l1_ms,
                chunk_background_l1_ms,
                chunk_tail_l1_ms,
                chunk_background_missed_deadline,
                chunk_repair_stop_ms,
                stop_latency_delta_ms,
                output_identical,
            };
            serde_json::to_writer(&mut writer, &row).map_err(|error| error.to_string())?;
            writer.write_all(b"\n").map_err(|error| error.to_string())?;
            writer.flush().map_err(|error| error.to_string())?;
            writer
                .get_ref()
                .sync_data()
                .map_err(|error| error.to_string())?;
            println!(
                "{}/{} {} windows={} full_stop={:.1}ms chunk_stop={:.1}ms delta={:+.1}ms identical={}",
                index + 1,
                EXPECTED_CLIPS,
                row.id,
                row.background_windows,
                row.full_final_stop_ms,
                row.chunk_repair_stop_ms,
                row.stop_latency_delta_ms,
                row.output_identical
            );
        }
        drop(writer);
        std::fs::rename(output_temp, output_path).map_err(|error| error.to_string())?;
        Ok(())
    }

    fn configure_qwen_only(output_path: &str) -> Result<(), String> {
        let app_data = Path::new(output_path)
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("app-data");
        std::fs::create_dir_all(&app_data).map_err(|error| error.to_string())?;
        std::env::set_var("RIGHTKIT_APP_DATA_DIR", app_data);
        std::env::set_var("RIGHTKIT_ENGINE_TEST_MODE", "1");
        std::env::set_var("RIGHTKIT_L3_CLEANUP", "1");
        std::env::set_var("RIGHTKIT_L3_CLOUD_CONSENT", "1");
        std::env::set_var("RIGHTKIT_L3_GROQ_CLEANUP_MODEL", GROQ_PRIMARY);
        std::env::set_var(
            "RIGHTKIT_L3_GROQ_CLEANUP_SAME_PROVIDER_FALLBACK_MODEL",
            GROQ_PRIMARY,
        );
        std::env::remove_var("CEREBRAS_API_KEY");
        std::env::remove_var("RIGHTKIT_L3_ROSTER");
        Ok(())
    }

    fn decode_exact_static15(
        model: &mut AsrRuntime,
        row: ManifestRow,
        audio: &[f32],
    ) -> Result<DecodedClip, String> {
        let duration_s = audio.len() as f64 / SAMPLE_RATE as f64;
        let mut transcript = String::new();
        let mut chunks = Vec::new();
        let mut start = 0usize;
        let mut main_available_at_s = 0.0f64;
        let mut total_asr_ms = 0.0f64;

        while let Some(segment) = scheduled_static15_ready_segment(audio, start) {
            let eligible_at_s = (start + 15 * SAMPLE_RATE) as f64 / SAMPLE_RATE as f64;
            let started = Instant::now();
            let text = model.transcribe_result(&audio[segment.clone()])?.text;
            let asr_ms = started.elapsed().as_secs_f64() * 1_000.0;
            main_available_at_s = main_available_at_s.max(eligible_at_s) + asr_ms / 1_000.0;
            total_asr_ms += asr_ms;
            append_scheduled_static15_text(&mut transcript, &text);
            chunks.push(DecodedChunk {
                kind: "background",
                start_s: segment.start as f64 / SAMPLE_RATE as f64,
                end_s: segment.end as f64 / SAMPLE_RATE as f64,
                eligible_at_s,
                asr_ms,
                asr_finished_at_s: main_available_at_s,
                text,
            });
            start = segment.end;
        }

        let started = Instant::now();
        let tail_text = model.transcribe_result(&audio[start..])?.text;
        let tail_asr_ms = started.elapsed().as_secs_f64() * 1_000.0;
        main_available_at_s = main_available_at_s.max(duration_s) + tail_asr_ms / 1_000.0;
        total_asr_ms += tail_asr_ms;
        append_scheduled_static15_text(&mut transcript, &tail_text);
        chunks.push(DecodedChunk {
            kind: "tail",
            start_s: start as f64 / SAMPLE_RATE as f64,
            end_s: duration_s,
            eligible_at_s: duration_s,
            asr_ms: tail_asr_ms,
            asr_finished_at_s: main_available_at_s,
            text: tail_text,
        });

        Ok(DecodedClip {
            row,
            chunks,
            transcript: transcript.trim().to_string(),
            total_asr_ms,
            stop_asr_ms: (main_available_at_s - duration_s).max(0.0) * 1_000.0,
        })
    }

    fn polish_once(input: &str, context: &PolishContext) -> PolishResult {
        let started = Instant::now();
        let outcome = app_polish_outcome(input, context);
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        match outcome {
            CleanupOutcome::Cleaned(text) => PolishResult {
                status: "cleaned",
                reason: None,
                elapsed_ms,
                text,
            },
            CleanupOutcome::Skipped { reason, .. } => PolishResult {
                status: "skipped_local_fallback",
                reason: Some(reason),
                elapsed_ms,
                text: input.to_string(),
            },
            CleanupOutcome::Failed { error_class, .. } => PolishResult {
                status: "failed_local_fallback",
                reason: Some(error_class),
                elapsed_ms,
                text: input.to_string(),
            },
        }
    }

    fn pace_provider() {
        std::thread::sleep(Duration::from_millis(PROVIDER_PACING_MS));
    }

    fn require_nonempty_env(name: &str) -> Result<(), String> {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|_| ())
            .ok_or_else(|| format!("{name} is required"))
    }

    fn load_wav(path: &Path) -> Result<Vec<f32>, String> {
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
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), String> {
    mac::run()
}
