//! Paired production-path Parakeet predictor-cache parity evaluation.
//!
//! The manifest is candidate-visible `{id,path}` data only. Each clip is run
//! serially through `transcribe_final_result_under_lease` with predictor cache
//! disabled, enabled, then disabled again. The repeated uncached arm separates
//! cache effects from ordinary backend non-determinism.
//!
//! ```text
//! cargo run --release --example parakeet_predictor_parity -- \
//!   <model-dir> <candidate-manifest.json> <output.json>
//! ```

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("parakeet_predictor_parity requires macOS Core ML");
    std::process::exit(2);
}

#[cfg(target_os = "macos")]
mod macos {
    use rightkit_dictation_engine::asr::AsrRuntime;
    use rightkit_dictation_engine::coreml_asr::CoreMlParakeet;
    use serde::{Deserialize, Serialize};
    use std::fs::{self, File};
    use std::io::{BufWriter, Write};
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    const SAMPLE_RATE: u32 = 16_000;
    const CACHE_ENV: &str = "RIGHTKIT_ASR_DISABLE_PREDICTOR_CACHE";

    #[derive(Debug, Clone, Deserialize)]
    struct InputRow {
        id: String,
        path: String,
    }

    #[derive(Debug, Clone, Serialize, PartialEq)]
    struct TokenRow {
        text: String,
        start_s: f32,
        end_s: f32,
    }

    #[derive(Debug, Clone, Serialize)]
    struct RunRow {
        cache: bool,
        elapsed_ms: f64,
        text: Option<String>,
        tokens: Vec<TokenRow>,
        error: Option<String>,
    }

    #[derive(Debug, Clone, Serialize)]
    struct ParityRow {
        manifest_index: usize,
        id: String,
        path: String,
        audio_samples: usize,
        audio_seconds: f64,
        uncached: RunRow,
        cached: RunRow,
        uncached_repeat: RunRow,
        cached_matches_uncached: bool,
        uncached_repeat_matches_uncached: bool,
    }

    #[derive(Debug, Serialize)]
    struct Output {
        schema: &'static str,
        model_dir: String,
        manifest: String,
        manifest_rows: usize,
        sample_rate_hz: u32,
        load_ms: f64,
        warmup_ms: f64,
        rows_completed: usize,
        rows: Vec<ParityRow>,
    }

    fn read_wav(path: &Path) -> Result<Vec<f32>, String> {
        let wav = rightkit_audio::wav::read_wav_mono_f32(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if wav.sample_rate != (SAMPLE_RATE) {
            return Err(format!(
                "{}: expected {} Hz WAV, got {} Hz",
                path.display(),
                SAMPLE_RATE,
                wav.sample_rate
            ));
        }
        Ok(wav.samples)
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

    fn decode_once(runtime: &mut AsrRuntime, audio: &[f32], cache: bool) -> RunRow {
        if cache {
            std::env::remove_var(CACHE_ENV);
        } else {
            std::env::set_var(CACHE_ENV, "1");
        }
        let started = Instant::now();
        let result = runtime.transcribe_final_result_under_lease(audio);
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        match result {
            Ok(value) => RunRow {
                cache,
                elapsed_ms,
                text: Some(value.text),
                tokens: token_rows(&value.tokens),
                error: None,
            },
            Err(error) => RunRow {
                cache,
                elapsed_ms,
                text: None,
                tokens: Vec::new(),
                error: Some(error),
            },
        }
    }

    fn same_transcript(left: &RunRow, right: &RunRow) -> bool {
        left.text == right.text && left.tokens == right.tokens && left.error == right.error
    }

    fn error_run(cache: bool, error: &str) -> RunRow {
        RunRow {
            cache,
            elapsed_ms: 0.0,
            text: None,
            tokens: Vec::new(),
            error: Some(error.to_string()),
        }
    }

    fn write_snapshot(output_path: &Path, output: &Output) -> Result<(), String> {
        let file = File::create(output_path).map_err(|e| e.to_string())?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, output).map_err(|e| e.to_string())?;
        writer.write_all(b"\n").map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
        writer.get_ref().sync_data().map_err(|e| e.to_string())
    }

    fn parse_args() -> Result<(PathBuf, PathBuf, PathBuf), String> {
        let args: Vec<_> = std::env::args().skip(1).collect();
        if args.len() != 3 {
            return Err(
                "usage: parakeet_predictor_parity <model-dir> <candidate-manifest.json> <output.json>"
                    .into(),
            );
        }
        Ok((
            PathBuf::from(&args[0]),
            PathBuf::from(&args[1]),
            PathBuf::from(&args[2]),
        ))
    }

    pub fn run() -> Result<(), String> {
        let (model_dir, manifest_path, output_path) = parse_args()?;
        if output_path.exists() {
            return Err(format!("output already exists: {}", output_path.display()));
        }
        let manifest_text = fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
        let inputs: Vec<InputRow> = serde_json::from_str(&manifest_text)
            .map_err(|e| format!("manifest must be a JSON array: {e}"))?;
        if inputs.is_empty() {
            return Err("manifest has no rows".into());
        }
        let mut ids = std::collections::HashSet::with_capacity(inputs.len());
        for row in &inputs {
            if row.id.is_empty() || row.path.is_empty() {
                return Err("manifest rows require non-empty id and path".into());
            }
            if !ids.insert(row.id.clone()) {
                return Err(format!("duplicate manifest id: {}", row.id));
            }
        }
        if let Some(parent) = output_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        eprintln!(
            "parakeet_predictor_parity start manifest_rows={} model={}",
            inputs.len(),
            model_dir.display()
        );
        let load_started = Instant::now();
        let model = CoreMlParakeet::load(&model_dir)?;
        let load_ms = load_started.elapsed().as_secs_f64() * 1_000.0;
        let mut runtime = AsrRuntime::CoreMlParakeet(model);

        // Exclude model/ANE setup from per-clip timing. Warm one real manifest
        // clip in each arm; silence can short-circuit a different path.
        let warm_audio = read_wav(Path::new(&inputs[0].path)).ok();
        let warm_started = Instant::now();
        if let Some(audio) = warm_audio.as_deref() {
            let _ = decode_once(&mut runtime, audio, false);
            let _ = decode_once(&mut runtime, audio, true);
            let _ = decode_once(&mut runtime, audio, false);
        }
        let warmup_ms = warm_started.elapsed().as_secs_f64() * 1_000.0;

        let mut output = Output {
            schema: "heardright.parakeet_predictor_parity.v1",
            model_dir: model_dir.display().to_string(),
            manifest: manifest_path.display().to_string(),
            manifest_rows: inputs.len(),
            sample_rate_hz: SAMPLE_RATE,
            load_ms,
            warmup_ms,
            rows_completed: 0,
            rows: Vec::with_capacity(inputs.len()),
        };
        write_snapshot(&output_path, &output)?;

        for (manifest_index, input) in inputs.iter().enumerate() {
            eprintln!(
                "parakeet_predictor_parity clip {}/{} id={}",
                manifest_index + 1,
                inputs.len(),
                input.id
            );
            let audio = match read_wav(Path::new(&input.path)) {
                Ok(audio) => audio,
                Err(error) => {
                    let uncached = error_run(false, &error);
                    let cached = error_run(true, &error);
                    let uncached_repeat = error_run(false, &error);
                    output.rows.push(ParityRow {
                        manifest_index,
                        id: input.id.clone(),
                        path: input.path.clone(),
                        audio_samples: 0,
                        audio_seconds: 0.0,
                        uncached,
                        cached,
                        uncached_repeat,
                        cached_matches_uncached: false,
                        uncached_repeat_matches_uncached: false,
                    });
                    output.rows_completed += 1;
                    write_snapshot(&output_path, &output)?;
                    std::env::remove_var(CACHE_ENV);
                    return Err(format!("clip {} audio read failed: {error}", input.id));
                }
            };
            let audio_seconds = audio.len() as f64 / SAMPLE_RATE as f64;
            let uncached = decode_once(&mut runtime, &audio, false);
            let cached = decode_once(&mut runtime, &audio, true);
            let uncached_repeat = decode_once(&mut runtime, &audio, false);
            let cached_matches_uncached = same_transcript(&cached, &uncached);
            let uncached_repeat_matches_uncached = same_transcript(&uncached_repeat, &uncached);
            let decode_error = [&uncached, &cached, &uncached_repeat]
                .iter()
                .find_map(|run| run.error.clone());
            output.rows.push(ParityRow {
                manifest_index,
                id: input.id.clone(),
                path: input.path.clone(),
                audio_samples: audio.len(),
                audio_seconds,
                uncached,
                cached,
                uncached_repeat,
                cached_matches_uncached,
                uncached_repeat_matches_uncached,
            });
            output.rows_completed += 1;
            write_snapshot(&output_path, &output)?;
            if let Some(error) = decode_error {
                std::env::remove_var(CACHE_ENV);
                return Err(format!("clip {} decode failed: {error}", input.id));
            }
        }
        std::env::remove_var(CACHE_ENV);
        eprintln!(
            "parakeet_predictor_parity complete rows={} output={}",
            output.rows_completed,
            output_path.display()
        );
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), String> {
    macos::run()
}
