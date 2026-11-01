//! Production-entry Windows Parakeet predictor reuse validation.
//!
//! Runs cached/uncached split graph modes and fused control through
//! `AsrRuntime::transcribe_final_result_under_lease` on identical manifest PCM.
//! Output stores transcripts and timed tokens for exact comparison; performance
//! is summarized only after all rows complete.
//!
//! Usage:
//!   windows_predictor_parity <model-dir> <manifest.json> <output.json> [row-limit]

#[cfg(target_os = "windows")]
mod windows_experiment {
    use rightkit_dictation_inference::asr::{AsrEp, AsrRuntime};
    use serde::{Deserialize, Serialize};
    use std::fs::{self, File};
    use std::io::{BufWriter, Write};
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    const SAMPLE_RATE: u32 = 16_000;
    const MODES: [&str; 4] = [
        "fused_baseline",
        "split_uncached",
        "split_cached",
        "fused_repeat",
    ];

    fn runtime_mode(label: &str) -> &'static str {
        match label {
            "fused_baseline" | "fused_repeat" => "fused",
            "split_uncached" => "split_uncached",
            "split_cached" => "split_cached",
            _ => "fused",
        }
    }

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

    #[derive(Debug, Clone, Serialize, PartialEq)]
    struct RunRow {
        mode: String,
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
        runs: Vec<RunRow>,
        all_outputs_match: bool,
    }

    #[derive(Debug, Serialize)]
    struct Output {
        schema: &'static str,
        model_dir: String,
        manifest: String,
        manifest_rows: usize,
        rows_requested: usize,
        sample_rate_hz: u32,
        encoder_provider: &'static str,
        predictor_joint_provider: &'static str,
        duration_cap: usize,
        split_graph_dir: String,
        rows_completed: usize,
        rows: Vec<ParityRow>,
    }

    fn read_wav(path: &Path) -> Result<Vec<f32>, String> {
        let wav = rightkit_audio::wav::read_wav_mono_f32(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if wav.sample_rate != (SAMPLE_RATE) as u32 {
            return Err(format!(
                "{}: expected {} Hz WAV, got {} Hz",
                path.display(),
                SAMPLE_RATE,
                wav.sample_rate
            ));
        }
        Ok(wav.samples)
    }

    fn decode_once(runtime: &mut AsrRuntime, audio: &[f32], mode: &str) -> RunRow {
        let experiment_mode = runtime_mode(mode);
        std::env::set_var("RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT", experiment_mode);
        eprintln!("experiment_run mode={mode} runtime_mode={experiment_mode}");
        let started = Instant::now();
        let result = runtime.transcribe_final_result_under_lease(audio);
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        match result {
            Ok(value) => RunRow {
                mode: mode.to_owned(),
                elapsed_ms,
                text: Some(value.text),
                tokens: value
                    .tokens
                    .into_iter()
                    .map(|token| TokenRow {
                        text: token.text,
                        start_s: token.start,
                        end_s: token.end,
                    })
                    .collect(),
                error: None,
            },
            Err(error) => RunRow {
                mode: mode.to_owned(),
                elapsed_ms,
                text: None,
                tokens: Vec::new(),
                error: Some(error),
            },
        }
    }

    fn same_output(left: &RunRow, right: &RunRow) -> bool {
        left.text == right.text && left.tokens == right.tokens && left.error == right.error
    }

    fn write_snapshot(output_path: &Path, output: &Output) -> Result<(), String> {
        let file = File::create(output_path).map_err(|e| e.to_string())?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, output).map_err(|e| e.to_string())?;
        writer.write_all(b"\n").map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
        writer.get_ref().sync_data().map_err(|e| e.to_string())
    }

    fn parse_args() -> Result<(PathBuf, PathBuf, PathBuf, Option<usize>), String> {
        let args: Vec<_> = std::env::args().skip(1).collect();
        if args.len() < 3 || args.len() > 4 {
            return Err("usage: windows_predictor_parity <model-dir> <manifest.json> <output.json> [row-limit]".into());
        }
        let limit = args
            .get(3)
            .map(|value| value.parse::<usize>().map_err(|e| e.to_string()))
            .transpose()?;
        Ok((
            PathBuf::from(&args[0]),
            PathBuf::from(&args[1]),
            PathBuf::from(&args[2]),
            limit,
        ))
    }

    fn run() -> Result<(), String> {
        let (model_dir, manifest_path, output_path, row_limit) = parse_args()?;
        if output_path.exists() {
            return Err(format!("output already exists: {}", output_path.display()));
        }
        let text = fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
        let inputs: Vec<InputRow> = serde_json::from_str(&text)
            .map_err(|e| format!("manifest must be a JSON array: {e}"))?;
        if inputs.is_empty() {
            return Err("manifest has no rows".into());
        }
        let rows_requested = row_limit.unwrap_or(inputs.len()).min(inputs.len());
        if rows_requested == 0 {
            return Err("row limit must be positive".into());
        }
        let mut ids = std::collections::HashSet::with_capacity(inputs.len());
        for row in &inputs {
            if row.id.is_empty() || row.path.is_empty() || !ids.insert(row.id.clone()) {
                return Err(format!("invalid or duplicate manifest row id: {}", row.id));
            }
        }
        if let Some(parent) = output_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        std::env::set_var("RIGHTKIT_ASR_BACKEND", "parakeet-tdt");
        std::env::set_var("RIGHTKIT_DICTATION_LANG", "auto");
        std::env::set_var("RIGHTKIT_TDT_DURATION_CAP", "2");
        if std::env::var_os("RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT_DIR").is_none() {
            return Err(
                "RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT_DIR must point at the audit split models"
                    .to_owned(),
            );
        }

        eprintln!(
            "windows_predictor_parity start manifest_rows={} requested={} model={}",
            inputs.len(),
            rows_requested,
            model_dir.display()
        );
        let load_started = Instant::now();
        let mut runtime = AsrRuntime::load(&model_dir, AsrEp::Dml)?;
        let load_ms = load_started.elapsed().as_secs_f64() * 1_000.0;
        eprintln!("runtime_load_ms={load_ms:.1} ep=dml duration_cap=2");

        // Warm each arm once on same first manifest clip; exclude warmups from rows.
        let warm_path = Path::new(&inputs[0].path);
        let warm_audio = read_wav(warm_path)?;
        for mode in MODES {
            let warm = decode_once(&mut runtime, &warm_audio, mode);
            if let Some(error) = warm.error {
                return Err(format!("warmup failed for {mode}: {error}"));
            }
        }
        std::env::set_var("RIGHTKIT_PREDICTOR_BENCH", "1");

        let mut output = Output {
            schema: "heardright.windows_predictor_parity.v1",
            model_dir: model_dir.display().to_string(),
            manifest: manifest_path.display().to_string(),
            manifest_rows: inputs.len(),
            rows_requested,
            sample_rate_hz: SAMPLE_RATE,
            encoder_provider: "DirectML",
            predictor_joint_provider: "CPUExecutionProvider",
            duration_cap: 2,
            split_graph_dir: std::env::var("RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT_DIR")
                .unwrap_or_default(),
            rows_completed: 0,
            rows: Vec::with_capacity(rows_requested),
        };
        write_snapshot(&output_path, &output)?;

        for (manifest_index, input) in inputs.iter().take(rows_requested).enumerate() {
            eprintln!(
                "windows_predictor_parity clip {}/{} id={}",
                manifest_index + 1,
                rows_requested,
                input.id
            );
            let audio = read_wav(Path::new(&input.path))
                .map_err(|error| format!("clip {} audio read failed: {error}", input.id))?;
            let audio_seconds = audio.len() as f64 / SAMPLE_RATE as f64;
            let rotation = manifest_index % MODES.len();
            let order = (0..MODES.len()).map(|offset| MODES[(offset + rotation) % MODES.len()]);
            let mut runs = Vec::with_capacity(MODES.len());
            for mode in order {
                runs.push(decode_once(&mut runtime, &audio, mode));
            }
            if let Some(error) = runs.iter().find_map(|run| run.error.clone()) {
                let row = ParityRow {
                    manifest_index,
                    id: input.id.clone(),
                    path: input.path.clone(),
                    audio_samples: audio.len(),
                    audio_seconds,
                    runs,
                    all_outputs_match: false,
                };
                output.rows.push(row);
                output.rows_completed += 1;
                write_snapshot(&output_path, &output)?;
                return Err(format!("clip {} decode failed: {error}", input.id));
            }
            let all_outputs_match = runs.iter().skip(1).all(|run| same_output(&runs[0], run));
            output.rows.push(ParityRow {
                manifest_index,
                id: input.id.clone(),
                path: input.path.clone(),
                audio_samples: audio.len(),
                audio_seconds,
                runs,
                all_outputs_match,
            });
            output.rows_completed += 1;
            write_snapshot(&output_path, &output)?;
        }
        std::env::remove_var("RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT");
        eprintln!(
            "windows_predictor_parity complete rows={} output={}",
            output.rows_completed,
            output_path.display()
        );
        Ok(())
    }

    pub fn entry() {
        if let Err(error) = run() {
            eprintln!("windows_predictor_parity error: {error}");
            std::process::exit(2);
        }
    }
}

#[cfg(target_os = "windows")]
fn main() {
    windows_experiment::entry();
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("windows_predictor_parity requires Windows");
    std::process::exit(2);
}
