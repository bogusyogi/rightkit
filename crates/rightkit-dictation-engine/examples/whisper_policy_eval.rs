//! Candidate-isolated Whisper policy replay over an opaque `{id,path}` JSONL.
//!
//! This keeps production's macOS CoreML Whisper + static-15 scheduler/final
//! assembly path, while selecting exactly one timestamp policy per process:
//! `baseline`, `no_mass`, or `text_only`. The input has no transcript or
//! scorer fields by construction.

#[cfg(target_os = "macos")]
mod macos {
    use rightkit_dictation_engine::asr::{
        apply_utterance_bias, scheduled_static15_ready_segment, AsrRuntime, ScheduledStatic15,
        AUDIBLE_BLANK_TRANSCRIPTION_ERROR,
    };
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use std::fs::{self, File};
    use std::io::{BufWriter, Write};
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    const SAMPLE_RATE: usize = 16_000;
    const MAX_CONSECUTIVE_ERRORS: usize = 3;

    #[derive(Debug, Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct InputRow {
        id: String,
        path: String,
    }

    #[derive(Debug, Clone, Copy)]
    enum Mode {
        Baseline,
        NoMass,
        TextOnly,
    }

    impl Mode {
        fn parse(value: &str) -> Result<Self, String> {
            match value {
                "baseline" => Ok(Self::Baseline),
                "no_mass" => Ok(Self::NoMass),
                "text_only" => Ok(Self::TextOnly),
                other => Err(format!(
                    "unknown mode {other:?}; expected baseline, no_mass, or text_only"
                )),
            }
        }

        fn name(self) -> &'static str {
            match self {
                Self::Baseline => "baseline",
                Self::NoMass => "no_mass",
                Self::TextOnly => "text_only",
            }
        }
    }

    #[derive(Debug)]
    struct Options {
        model: PathBuf,
        manifest: PathBuf,
        output: PathBuf,
        mode: Mode,
        start: usize,
        end: Option<usize>,
        limit: Option<usize>,
        preconditioned: bool,
    }

    #[derive(Debug, Clone, Serialize)]
    struct TokenRow {
        text: String,
        start_s: f32,
        end_s: f32,
    }

    #[derive(Debug, Clone, Serialize)]
    struct AttemptRow {
        elapsed_ms: f64,
        raw_samples: usize,
        decode_samples: usize,
        text: Option<String>,
        token_count: usize,
        error: Option<String>,
    }

    #[derive(Debug, Clone, Serialize)]
    struct DecodeRow {
        elapsed_ms: f64,
        retries: usize,
        text: Option<String>,
        token_count: usize,
        tokens: Vec<TokenRow>,
        attempts: Vec<AttemptRow>,
        error: Option<String>,
    }

    #[derive(Debug, Clone, Serialize)]
    struct ChunkRow {
        kind: &'static str,
        start_sample: usize,
        end_sample: usize,
        raw_samples: usize,
        decode_samples: usize,
        decode: DecodeRow,
    }

    #[derive(Debug, Serialize)]
    struct OutputRow {
        schema: &'static str,
        mode: &'static str,
        manifest_index: usize,
        id: String,
        audio_samples: usize,
        sample_rate: usize,
        preconditioned: bool,
        background_windows: usize,
        committed_samples_before_final: usize,
        tail_samples: usize,
        language_lock: Option<i32>,
        chunks: Vec<ChunkRow>,
        final_decode: Option<DecodeRow>,
        finalization_elapsed_ms: f64,
        assembled_text: Option<String>,
        error: Option<String>,
        background_error: Option<String>,
    }

    #[derive(Debug)]
    struct Decode {
        result: Result<rightkit_dictation_inference::TranscriptionResult, String>,
        row: DecodeRow,
    }

    fn parse_options() -> Result<Options, String> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.len() < 5 {
            return Err("usage: whisper_policy_eval MODEL INPUTS_JSONL OUTPUT_JSONL --mode MODE [--start N] [--end N] [--limit N] [--preconditioned]".into());
        }
        let model = PathBuf::from(&args[0]);
        let manifest = PathBuf::from(&args[1]);
        let output = PathBuf::from(&args[2]);
        let mut mode = None;
        let mut start = 0usize;
        let mut end = None;
        let mut limit = None;
        let mut preconditioned = false;
        let mut i = 3;
        while i < args.len() {
            match args[i].as_str() {
                "--mode" => {
                    i += 1;
                    mode = Some(Mode::parse(args.get(i).ok_or("--mode needs a value")?)?);
                }
                "--start" => {
                    i += 1;
                    start = args
                        .get(i)
                        .ok_or("--start needs a value")?
                        .parse()
                        .map_err(|_| "--start must be an integer")?;
                }
                "--end" => {
                    i += 1;
                    end = Some(
                        args.get(i)
                            .ok_or("--end needs a value")?
                            .parse()
                            .map_err(|_| "--end must be an integer")?,
                    );
                }
                "--limit" => {
                    i += 1;
                    limit = Some(
                        args.get(i)
                            .ok_or("--limit needs a value")?
                            .parse()
                            .map_err(|_| "--limit must be an integer")?,
                    );
                }
                "--preconditioned" => preconditioned = true,
                other => return Err(format!("unknown option {other:?}")),
            }
            i += 1;
        }
        Ok(Options {
            model,
            manifest,
            output,
            mode: mode.ok_or("--mode is required")?,
            start,
            end,
            limit,
            preconditioned,
        })
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

    fn configure_mode(mode: Mode, model: &Path) {
        std::env::set_var("RIGHTKIT_ASR_BACKEND", "whisper");
        std::env::set_var("RIGHTKIT_ASR_COREML", "1");
        std::env::set_var("RIGHTKIT_DICTATION_LANG", "auto");
        std::env::set_var("RIGHTKIT_WHISPER_COREML_MODEL", model);
        std::env::remove_var("RIGHTKIT_WHISPER_DISABLE_TIMESTAMP_MASS");
        std::env::remove_var("RIGHTKIT_WHISPER_NOTS");
        match mode {
            Mode::Baseline => {}
            Mode::NoMass => std::env::set_var("RIGHTKIT_WHISPER_DISABLE_TIMESTAMP_MASS", "1"),
            Mode::TextOnly => std::env::set_var("RIGHTKIT_WHISPER_NOTS", "1"),
        }
    }

    fn token_rows(tokens: &[rightkit_dictation_inference::TimedToken]) -> Vec<TokenRow> {
        tokens
            .iter()
            .map(|token| TokenRow {
                text: token.text.clone(),
                start_s: token.start,
                end_s: token.end,
            })
            .collect()
    }

    fn decode_segment(
        runtime: &mut AsrRuntime,
        raw: &[f32],
        locked_lang: &mut Option<i32>,
        preconditioned: bool,
        owner: &'static str,
    ) -> Decode {
        let audio = if preconditioned {
            raw.to_vec()
        } else {
            let policy = std::env::var("RIGHTKIT_AUDIO_CONDITIONING_POLICY")
                .unwrap_or_else(|_| "default".to_string());
            rightkit_dictation_core::audio_conditioning::condition_for_asr(
                raw,
                SAMPLE_RATE as u32,
                &policy,
            )
        };
        let mut attempts = Vec::new();
        let mut retries = 0usize;
        let mut result = decode_once(
            runtime,
            &audio,
            raw.len(),
            locked_lang,
            owner,
            &mut attempts,
        );
        if matches!(&result, Ok(value) if value.text.trim().is_empty())
            || matches!(&result, Err(error) if error == AUDIBLE_BLANK_TRANSCRIPTION_ERROR)
        {
            retries = 1;
            result = decode_once(
                runtime,
                &audio,
                raw.len(),
                locked_lang,
                owner,
                &mut attempts,
            );
        }
        if matches!(&result, Ok(value) if value.text.trim().is_empty()) {
            result = Err(AUDIBLE_BLANK_TRANSCRIPTION_ERROR.to_string());
        }
        let elapsed_ms = attempts.iter().map(|attempt| attempt.elapsed_ms).sum();
        let (text, tokens, token_count, error) = match &result {
            Ok(value) => (
                Some(value.text.clone()),
                token_rows(&value.tokens),
                value.tokens.len(),
                None,
            ),
            Err(error) => (None, Vec::new(), 0, Some(error.clone())),
        };
        Decode {
            result,
            row: DecodeRow {
                elapsed_ms,
                retries,
                text,
                token_count,
                tokens,
                attempts,
                error,
            },
        }
    }

    fn decode_once(
        runtime: &mut AsrRuntime,
        audio: &[f32],
        raw_samples: usize,
        locked_lang: &mut Option<i32>,
        owner: &'static str,
        attempts: &mut Vec<AttemptRow>,
    ) -> Result<rightkit_dictation_inference::TranscriptionResult, String> {
        let started = Instant::now();
        let result = runtime.with_inference_lease(owner, |model| {
            apply_utterance_bias(model);
            model.transcribe_final_result_under_lease_locked(audio, locked_lang)
        });
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let (text, token_count, error) = match &result {
            Ok(value) => (Some(value.text.clone()), value.tokens.len(), None),
            Err(error) => (None, 0, Some(error.clone())),
        };
        attempts.push(AttemptRow {
            elapsed_ms,
            raw_samples,
            decode_samples: audio.len(),
            text,
            token_count,
            error,
        });
        result
    }

    fn lifecycle(value: serde_json::Value) {
        println!("{}", value);
        let _ = std::io::stdout().flush();
    }

    fn write_row(writer: &mut BufWriter<File>, row: &OutputRow) -> Result<(), String> {
        serde_json::to_writer(&mut *writer, row).map_err(|e| e.to_string())?;
        writer.write_all(b"\n").map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
        writer.get_ref().sync_data().map_err(|e| e.to_string())
    }

    pub fn run() -> Result<(), String> {
        let options = parse_options()?;
        let rows: Vec<InputRow> = fs::read_to_string(&options.manifest)
            .map_err(|e| e.to_string())?
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        let end = options.end.unwrap_or(rows.len());
        let end = end.min(rows.len());
        if options.start > end {
            return Err("--start must be <= --end".into());
        }
        let selected_end = options
            .limit
            .map_or(end, |limit| end.min(options.start.saturating_add(limit)));
        if options.output.exists() {
            return Err(format!(
                "output already exists: {}",
                options.output.display()
            ));
        }

        configure_mode(options.mode, &options.model);
        lifecycle(json!({
            "event":"start",
            "schema":"heardright.whisper_policy_eval.v1",
            "mode":options.mode.name(),
            "manifest_rows":rows.len(),
            "start":options.start,
            "end":selected_end,
            "limit":options.limit,
            "preconditioned":options.preconditioned,
        }));
        let load_started = Instant::now();
        let engine =
            rightkit_dictation_inference::whisper_coreml::WhisperCoreMl::load(&options.model)?;
        let load_ms = load_started.elapsed().as_secs_f64() * 1_000.0;
        let lang_tok = engine.lang_token("auto");
        let mut runtime = AsrRuntime::WhisperCoreMl { engine, lang_tok };
        if !runtime.uses_scheduled_static15() {
            return Err("Whisper runtime did not select production static-15 scheduling".into());
        }
        lifecycle(
            json!({"event":"model_loaded","load_ms":load_ms,"runtime":"WhisperCoreMl","scheduled_static15":true}),
        );
        let warm_started = Instant::now();
        let warm_result = runtime.with_inference_lease("policy_eval_warmup", |model| {
            apply_utterance_bias(model);
            model.transcribe_under_lease(&vec![0.0; SAMPLE_RATE])
        });
        let warm_ms = warm_started.elapsed().as_secs_f64() * 1_000.0;
        lifecycle(
            json!({"event":"warmup_complete","warm_ms":warm_ms,"ok":warm_result.is_ok(),"error":warm_result.as_ref().err()}),
        );
        warm_result.map_err(|e| format!("warmup failed: {e}"))?;

        if let Some(parent) = options.output.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut writer = BufWriter::new(File::create(&options.output).map_err(|e| e.to_string())?);
        let mut consecutive_errors = 0usize;
        for (ordinal, manifest_index) in (options.start..selected_end).enumerate() {
            let row = &rows[manifest_index];
            lifecycle(
                json!({"event":"clip_start","ordinal":ordinal,"manifest_index":manifest_index,"id":row.id}),
            );
            let clip_started = Instant::now();
            let mut locked_lang = None;
            let mut chunks = Vec::new();
            let mut background_error = None;
            let mut assembled_text = None;
            let mut final_decode = None;
            let mut finalization_elapsed_ms = 0.0;
            let mut audio_samples = 0usize;
            let mut committed_samples_before_final = 0usize;
            let mut tail_samples = 0usize;
            let mut row_error = None;

            match read_wav(Path::new(&row.path)) {
                Err(error) => row_error = Some(error),
                Ok(audio) => {
                    audio_samples = audio.len();
                    let mut scheduled = ScheduledStatic15::default();
                    while let Some(segment) =
                        scheduled_static15_ready_segment(&audio, scheduled.committed_samples())
                    {
                        let start_sample = segment.start;
                        let end_sample = segment.end;
                        let decoded = decode_segment(
                            &mut runtime,
                            &audio[segment.clone()],
                            &mut locked_lang,
                            options.preconditioned,
                            "policy_eval_background",
                        );
                        let commit = match &decoded.result {
                            Ok(result) => scheduled.commit_window(
                                start_sample,
                                end_sample,
                                &result.text,
                                result.tokens.clone(),
                            ),
                            Err(error) => Err(error.clone()),
                        };
                        let chunk = ChunkRow {
                            kind: "background",
                            start_sample,
                            end_sample,
                            raw_samples: end_sample - start_sample,
                            decode_samples: decoded
                                .row
                                .attempts
                                .first()
                                .map_or(0, |attempt| attempt.decode_samples),
                            decode: decoded.row,
                        };
                        chunks.push(chunk);
                        if let Err(error) = commit {
                            background_error = Some(error);
                            break;
                        }
                    }
                    committed_samples_before_final = scheduled.committed_samples();
                    tail_samples = audio.len().saturating_sub(committed_samples_before_final);
                    let final_started = Instant::now();
                    let mut tail_decode = None;
                    let result = scheduled.finish_result(&audio, |tail| {
                        let decoded = decode_segment(
                            &mut runtime,
                            tail,
                            &mut locked_lang,
                            options.preconditioned,
                            "policy_eval_final",
                        );
                        let Decode { result, row } = decoded;
                        tail_decode = Some(row);
                        result
                    });
                    finalization_elapsed_ms = final_started.elapsed().as_secs_f64() * 1_000.0;
                    final_decode = tail_decode;
                    match result {
                        Ok(result) => assembled_text = Some(result.text),
                        Err(error) => row_error = Some(error),
                    }
                }
            }
            let row_failed = row_error.is_some();
            if row_failed {
                consecutive_errors += 1;
            } else {
                consecutive_errors = 0;
            }
            let output_row = OutputRow {
                schema: "heardright.whisper_policy_eval.v1",
                mode: options.mode.name(),
                manifest_index,
                id: row.id.clone(),
                audio_samples,
                sample_rate: SAMPLE_RATE,
                preconditioned: options.preconditioned,
                background_windows: chunks
                    .iter()
                    .filter(|chunk| chunk.kind == "background")
                    .count(),
                committed_samples_before_final,
                tail_samples,
                language_lock: locked_lang,
                chunks,
                final_decode,
                finalization_elapsed_ms,
                assembled_text,
                error: row_error,
                background_error,
            };
            write_row(&mut writer, &output_row)?;
            lifecycle(json!({
                "event":"clip_complete",
                "ordinal":ordinal,
                "manifest_index":manifest_index,
                "id":row.id,
                "elapsed_ms":clip_started.elapsed().as_secs_f64()*1_000.0,
                "ok":!row_failed,
                "consecutive_errors":consecutive_errors,
            }));
            if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                lifecycle(
                    json!({"event":"circuit_breaker","consecutive_errors":consecutive_errors,"completed":ordinal+1}),
                );
                return Err(format!(
                    "three consecutive clip errors after manifest index {manifest_index}"
                ));
            }
        }
        lifecycle(
            json!({"event":"complete","mode":options.mode.name(),"start":options.start,"end":selected_end}),
        );
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), String> {
    macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("whisper_policy_eval is macOS-only");
}
