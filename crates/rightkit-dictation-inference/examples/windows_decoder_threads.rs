//! Bounded Windows CPU-stage ORT thread-count probe through the production
//! Parakeet entry point. Encoder remains DirectML; only CPU mel and
//! decoder/joint session intra-op threads are varied.
//!
//! Screening runs four fresh sequential model loads: ORT default, CPU-stage
//! threads 1, CPU-stage threads 2, and ORT default again. `--confirm` runs the
//! canonical 224-clip confirmation with default, threads 1, and default repeat.
//! Each arm warms on the first clip, then transcribes the manifest in order.
//! References are never loaded. Outputs include transcript and timed-token
//! parity plus per-clip latency; the default repeat exposes run drift.
//!
//! Usage:
//!   windows_decoder_threads <model-dir> <manifest.json> <output.json> [--confirm]

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("windows_decoder_threads requires Windows");
    std::process::exit(2);
}

#[cfg(target_os = "windows")]
mod windows {
    use rightkit_dictation_inference::asr::{AsrEp, AsrRuntime};
    use serde::{Deserialize, Serialize};
    use std::fs::{self, File};
    use std::io::{BufWriter, Write};
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    const SAMPLE_RATE: u32 = 16_000;
    const SUBSET_SHA256: &str = "3b77ba9fa5d1ca3132c13034af04200f6f4e7c400debdf09e2f914cbd78b3d72";
    const CANONICAL_SHA256: &str =
        "f664090693561c9d4102cdb4a790f69103534e9022ef185178269eb71ce652b2";

    #[derive(Clone, Copy)]
    struct ArmSpec {
        label: &'static str,
        threads: Option<usize>,
    }

    const SCREEN_ARMS: [ArmSpec; 4] = [
        ArmSpec {
            label: "default",
            threads: None,
        },
        ArmSpec {
            label: "threads_1",
            threads: Some(1),
        },
        ArmSpec {
            label: "threads_2",
            threads: Some(2),
        },
        ArmSpec {
            label: "default_repeat",
            threads: None,
        },
    ];
    const CONFIRM_ARMS: [ArmSpec; 3] = [
        ArmSpec {
            label: "default",
            threads: None,
        },
        ArmSpec {
            label: "threads_1",
            threads: Some(1),
        },
        ArmSpec {
            label: "default_repeat",
            threads: None,
        },
    ];

    #[derive(Debug, Clone, Deserialize)]
    struct InputRow {
        stratum: Option<String>,
        id: String,
        path: String,
        duration_s: f64,
        sha256: String,
    }

    #[derive(Debug, Clone, Serialize, PartialEq)]
    struct TokenRow {
        text: String,
        start_s: f32,
        end_s: f32,
    }

    #[derive(Debug, Clone, Serialize, PartialEq)]
    struct DecodeOutput {
        text: String,
        tokens: Vec<TokenRow>,
    }

    #[derive(Debug, Clone, Serialize)]
    struct ClipResult {
        id: String,
        stratum: String,
        manifest_duration_s: f64,
        measured_duration_s: f64,
        elapsed_ms: f64,
        output: DecodeOutput,
        exact_text_with_default: Option<bool>,
        exact_timed_tokens_with_default: Option<bool>,
    }

    #[derive(Debug, Serialize)]
    struct ArmResult {
        label: String,
        cpu_stage_threads: Option<usize>,
        load_ms: f64,
        warmup_ms: f64,
        rows: Vec<ClipResult>,
        median_ms: Option<f64>,
        p90_ms: Option<f64>,
        median_paired_delta_ms: Option<f64>,
        exact_text_clips: Option<usize>,
        exact_timed_token_clips: Option<usize>,
    }

    #[derive(Debug, Serialize)]
    struct Output {
        schema: &'static str,
        model_dir: String,
        manifest_path: String,
        manifest_sha256: String,
        manifest_rows: usize,
        confirmation: bool,
        sample_rate_hz: u32,
        encoder_provider: &'static str,
        cpu_stage_provider: &'static str,
        beam_width: usize,
        duration_cap: usize,
        emission_reward: f32,
        available_parallelism: Option<usize>,
        arm_order: Vec<String>,
        arms: Vec<ArmResult>,
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

    fn sha256(bytes: &[u8]) -> String {
        rightkit_fs::digest::sha256_bytes(bytes)
    }

    fn stratum_for(duration_s: f64) -> &'static str {
        if duration_s <= 10.0 {
            "short"
        } else if duration_s <= 25.0 {
            "medium"
        } else {
            "long"
        }
    }

    fn validate_inputs(
        manifest: &Path,
        confirmation: bool,
    ) -> Result<(Vec<InputRow>, String), String> {
        let bytes = fs::read(manifest).map_err(|e| format!("read manifest: {e}"))?;
        let manifest_hash = sha256(&bytes);
        let expected_hash = if confirmation {
            CANONICAL_SHA256
        } else {
            SUBSET_SHA256
        };
        if !manifest_hash.eq_ignore_ascii_case(expected_hash) {
            return Err(format!(
                "manifest SHA-256 mismatch: expected {expected_hash}, got {manifest_hash}"
            ));
        }
        let inputs: Vec<InputRow> = serde_json::from_slice(&bytes)
            .map_err(|e| format!("manifest must be a JSON array: {e}"))?;
        let expected_rows = if confirmation { 224 } else { 12 };
        if inputs.len() != expected_rows {
            return Err(format!(
                "expected {expected_rows} frozen rows, got {}",
                inputs.len()
            ));
        }
        let mut ids = std::collections::HashSet::with_capacity(inputs.len());
        let mut strata = std::collections::HashMap::new();
        for row in &inputs {
            if row.id.is_empty() || row.path.is_empty() || !ids.insert(row.id.clone()) {
                return Err(format!("invalid or duplicate clip id: {}", row.id));
            }
            let actual_stratum = stratum_for(row.duration_s);
            if row
                .stratum
                .as_deref()
                .is_some_and(|stratum| stratum != actual_stratum)
            {
                return Err(format!("duration stratum mismatch for {}", row.id));
            }
            *strata.entry(actual_stratum).or_insert(0usize) += 1;
            let wav_path = Path::new(&row.path);
            let wav_bytes =
                fs::read(wav_path).map_err(|e| format!("read WAV {}: {e}", wav_path.display()))?;
            let actual = sha256(&wav_bytes);
            if !actual.eq_ignore_ascii_case(&row.sha256) {
                return Err(format!("WAV SHA-256 mismatch for {}", row.id));
            }
        }
        if confirmation {
            if strata.values().copied().sum::<usize>() != 224
                || ["short", "medium", "long"]
                    .iter()
                    .any(|stratum| !strata.contains_key(stratum))
            {
                return Err(
                    "canonical confirmation lacks complete duration-stratum coverage".into(),
                );
            }
        } else {
            for stratum in ["short", "medium", "long"] {
                if strata.get(stratum).copied() != Some(4) {
                    return Err(format!("expected exactly four {stratum} clips"));
                }
            }
            for fixture in ["6e3dda7ad0200079", "a86d5ba90b1e46a9"] {
                if !ids.contains(fixture) {
                    return Err(format!(
                        "required timed-token fixture is missing: {fixture}"
                    ));
                }
            }
        }
        Ok((inputs, manifest_hash))
    }

    fn decode(
        runtime: &mut AsrRuntime,
        audio: &[f32],
        id: &str,
    ) -> Result<(DecodeOutput, f64), String> {
        let started = Instant::now();
        let result = runtime
            .transcribe_final_result_under_lease(audio)
            .map_err(|error| format!("clip {id}: {error}"))?;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        Ok((
            DecodeOutput {
                text: result.text,
                tokens: result
                    .tokens
                    .into_iter()
                    .map(|token| TokenRow {
                        text: token.text,
                        start_s: token.start,
                        end_s: token.end,
                    })
                    .collect(),
            },
            elapsed_ms,
        ))
    }

    fn median(values: &[f64]) -> Option<f64> {
        if values.is_empty() {
            return None;
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        let middle = sorted.len() / 2;
        Some(if sorted.len() % 2 == 0 {
            (sorted[middle - 1] + sorted[middle]) / 2.0
        } else {
            sorted[middle]
        })
    }

    fn p90(values: &[f64]) -> Option<f64> {
        if values.is_empty() {
            return None;
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        Some(sorted[((sorted.len() * 9).div_ceil(10)).saturating_sub(1)])
    }

    fn write_snapshot(path: &Path, output: &Output) -> Result<(), String> {
        let file = File::create(path).map_err(|e| format!("create snapshot: {e}"))?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, output).map_err(|e| e.to_string())?;
        writer.write_all(b"\n").map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
        writer.get_ref().sync_data().map_err(|e| e.to_string())
    }

    fn parse_args() -> Result<(PathBuf, PathBuf, PathBuf, bool), String> {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if args.len() != 3 && args.len() != 4 {
            return Err(
                "usage: windows_decoder_threads <model-dir> <manifest.json> <output.json> [--confirm]"
                    .into(),
            );
        }
        let confirmation = match args.get(3) {
            None => false,
            Some(value) if value == std::ffi::OsStr::new("--confirm") => true,
            Some(_) => return Err("only the optional --confirm flag is supported".into()),
        };
        Ok((
            PathBuf::from(&args[0]),
            PathBuf::from(&args[1]),
            PathBuf::from(&args[2]),
            confirmation,
        ))
    }

    fn execute() -> Result<(), String> {
        let (model_dir, manifest_path, output_path, confirmation) = parse_args()?;
        if output_path.exists() {
            return Err(format!(
                "refusing to overwrite existing output: {}",
                output_path.display()
            ));
        }
        let (inputs, manifest_sha256) = validate_inputs(&manifest_path, confirmation)?;
        let arm_specs: &[ArmSpec] = if confirmation {
            &CONFIRM_ARMS
        } else {
            &SCREEN_ARMS
        };
        if let Some(parent) = output_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|e| format!("create output directory: {e}"))?;
        }

        std::env::set_var("RIGHTKIT_ASR_BACKEND", "parakeet-tdt");
        std::env::set_var("RIGHTKIT_DICTATION_LANG", "auto");
        std::env::set_var("RIGHTKIT_ASR_BEAM_WIDTH", "2");
        std::env::set_var("RIGHTKIT_ASR_BEAM_EMIT_REWARD", "0.10");
        std::env::set_var("RIGHTKIT_TDT_DURATION_CAP", "2");
        std::env::set_var("RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT", "fused");
        std::env::remove_var("RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT_DIR");

        let mut output = Output {
            schema: "heardright.windows_decoder_threads.v1",
            model_dir: model_dir.display().to_string(),
            manifest_path: manifest_path.display().to_string(),
            manifest_sha256,
            manifest_rows: inputs.len(),
            confirmation,
            sample_rate_hz: SAMPLE_RATE,
            encoder_provider: "DirectML",
            cpu_stage_provider: "CPUExecutionProvider (mel and decoder/joint)",
            beam_width: 2,
            duration_cap: 2,
            emission_reward: 0.10,
            available_parallelism: std::thread::available_parallelism().ok().map(usize::from),
            arm_order: arm_specs.iter().map(|arm| arm.label.to_owned()).collect(),
            arms: Vec::with_capacity(arm_specs.len()),
        };
        write_snapshot(&output_path, &output)?;

        for (arm_index, spec) in arm_specs.iter().enumerate() {
            let threads = spec.threads;
            let label = spec.label;
            match threads {
                Some(value) => {
                    std::env::set_var("RIGHTKIT_ONNX_CPU_THREADS_EXPERIMENT", value.to_string())
                }
                None => std::env::remove_var("RIGHTKIT_ONNX_CPU_THREADS_EXPERIMENT"),
            }
            eprintln!("RIGHTKIT_DECODER_THREADS_ARM_BEGIN label={label} threads={threads:?}");
            let load_started = Instant::now();
            let mut runtime = AsrRuntime::load(&model_dir, AsrEp::Dml)
                .map_err(|error| format!("load arm {label}: {error}"))?;
            let load_ms = load_started.elapsed().as_secs_f64() * 1_000.0;

            let warm_audio = read_wav(Path::new(&inputs[0].path))?;
            let (_, warmup_ms) = decode(&mut runtime, &warm_audio, "__warmup__")?;
            eprintln!(
                "RIGHTKIT_DECODER_THREADS_ARM_WARMED label={label} load_ms={load_ms:.1} warmup_ms={warmup_ms:.1}"
            );

            let mut arm = ArmResult {
                label: label.to_owned(),
                cpu_stage_threads: threads,
                load_ms,
                warmup_ms,
                rows: Vec::with_capacity(inputs.len()),
                median_ms: None,
                p90_ms: None,
                median_paired_delta_ms: None,
                exact_text_clips: None,
                exact_timed_token_clips: None,
            };

            for (clip_index, input) in inputs.iter().enumerate() {
                let audio = read_wav(Path::new(&input.path))?;
                if audio.is_empty() {
                    return Err(format!("clip {} is empty", input.id));
                }
                let measured_duration_s = audio.len() as f64 / SAMPLE_RATE as f64;
                let (decoded, elapsed_ms) = decode(&mut runtime, &audio, &input.id)?;
                let baseline = output
                    .arms
                    .first()
                    .and_then(|default_arm| default_arm.rows.get(clip_index));
                let clip = ClipResult {
                    id: input.id.clone(),
                    stratum: stratum_for(input.duration_s).to_owned(),
                    manifest_duration_s: input.duration_s,
                    measured_duration_s,
                    elapsed_ms,
                    exact_text_with_default: baseline.map(|row| row.output.text == decoded.text),
                    exact_timed_tokens_with_default: baseline
                        .map(|row| row.output.tokens == decoded.tokens),
                    output: decoded,
                };
                eprintln!(
                    "RIGHTKIT_DECODER_THREADS_CLIP label={label} completed={}/{} id={} elapsed_ms={:.1} tokens={}",
                    clip_index + 1,
                    inputs.len(),
                    clip.id,
                    clip.elapsed_ms,
                    clip.output.tokens.len()
                );
                arm.rows.push(clip);
                output.arms.truncate(arm_index);
                output.arms.push(arm);
                write_snapshot(&output_path, &output)?;
                arm = output.arms.pop().expect("just appended partial arm");
            }

            drop(runtime);
            let latencies: Vec<_> = arm.rows.iter().map(|row| row.elapsed_ms).collect();
            arm.median_ms = median(&latencies);
            arm.p90_ms = p90(&latencies);
            if arm_index == 0 {
                arm.exact_text_clips = Some(inputs.len());
                arm.exact_timed_token_clips = Some(inputs.len());
                arm.median_paired_delta_ms = Some(0.0);
            } else if let Some(default_arm) = output.arms.first() {
                let deltas: Vec<_> = arm
                    .rows
                    .iter()
                    .zip(default_arm.rows.iter())
                    .map(|(current, base)| current.elapsed_ms - base.elapsed_ms)
                    .collect();
                arm.median_paired_delta_ms = median(&deltas);
                arm.exact_text_clips = Some(
                    arm.rows
                        .iter()
                        .filter(|row| row.exact_text_with_default == Some(true))
                        .count(),
                );
                arm.exact_timed_token_clips = Some(
                    arm.rows
                        .iter()
                        .filter(|row| row.exact_timed_tokens_with_default == Some(true))
                        .count(),
                );
            }
            output.arms.truncate(arm_index);
            output.arms.push(arm);
            write_snapshot(&output_path, &output)?;
            eprintln!(
                "RIGHTKIT_DECODER_THREADS_ARM_COMPLETE label={label} median_ms={:.1} p90_ms={:.1}",
                output.arms[arm_index].median_ms.unwrap_or_default(),
                output.arms[arm_index].p90_ms.unwrap_or_default()
            );
        }
        std::env::remove_var("RIGHTKIT_ONNX_CPU_THREADS_EXPERIMENT");
        eprintln!(
            "RIGHTKIT_DECODER_THREADS_COMPLETE output={}",
            output_path.display()
        );
        Ok(())
    }

    pub fn main() {
        if let Err(error) = execute() {
            eprintln!("windows_decoder_threads error: {error}");
            std::process::exit(2);
        }
    }
}

#[cfg(target_os = "windows")]
fn main() {
    windows::main();
}
