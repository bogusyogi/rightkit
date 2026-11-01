//! Paired production-path hypothesis API batching evaluation.
//! Predictor reuse remains enabled in every arm; independent providers retain
//! each hypothesis state/frame. Rotated scalar, batch & scalar-repeat controls.
//! Process CPU & wall time exclude loading, sanity, warmup & WAV reads.
//! Run through RightKit after freezing candidate manifest & scorer.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("parakeet_hypothesis_batch requires macOS Core ML");
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
    const CACHE_ENV: &str = "RIGHTKIT_COREML_HYP_BATCH";
    const DURATION_CAP_ENV: &str = "RIGHTKIT_COREML_DURATION_CAP_PROBE";
    const MAX_SYMBOLS_ENV: &str = "RIGHTKIT_COREML_MAX_SYMBOLS_PROBE";

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
        batch: bool,
        elapsed_ms: f64,
        user_ms: f64,
        kernel_ms: f64,
        runtime_counts: [usize; 4],
        fused_counts: [usize; 2],
        output_backing_counts: [u64; 4],
        frontend_lookahead_counts: [usize; 4],
        unique_runtime_invocations: usize,
        logical_predictor_calls: usize,
        logical_joint_calls: usize,
        predictor_invocations: usize,
        joint_invocations: usize,
        predictor_cache_reuses: usize,
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
        current: RunRow,
        batched: RunRow,
        current_repeat: Option<RunRow>,
        batched_matches_current: bool,
        current_repeat_matches_current: Option<bool>,
    }

    #[derive(Debug, Serialize)]
    struct Output {
        schema: &'static str,
        paired_only: bool,
        model_dir: String,
        manifest: String,
        manifest_rows: usize,
        sample_rate_hz: u32,
        load_ms: f64,
        sanity_max_abs: [f32; 4],
        fused_sanity: Option<Vec<[f32; 9]>>,
        fused_buffer_sanity: Option<Vec<[f32; 9]>>,
        aligned_fused_sanity: Option<Vec<[f32; 9]>>,
        frontend_lookahead_preparation: Option<serde_json::Value>,
        buffer_reuse_sanity: Option<[f32; 4]>,
        bias_file: Option<String>,
        bias_score: Option<f32>,
        bias_installed: usize,
        beam_width: String,
        candidate_beam_width: Option<usize>,
        candidate_duration_cap: Option<String>,
        candidate_max_symbols: Option<String>,
        batch_mode: String,
        batch_max_rows: String,
        encoder_compute: String,
        decoder_compute: String,
        fused_compute: Option<String>,
        warmup_ms: f64,
        warmup_calls: usize,
        frontend_warmup_counts: [usize; 4],
        rows_completed: usize,
        rows: Vec<ParityRow>,
    }

    #[derive(Debug, Clone, Serialize)]
    struct ProductionRunRow {
        elapsed_ms: f64,
        user_ms: f64,
        kernel_ms: f64,
        runtime_counts: [usize; 4],
        fused_counts: [usize; 2],
        output_backing_counts: [u64; 4],
        frontend_lookahead_counts: [usize; 4],
        text: Option<String>,
        tokens: Vec<TokenRow>,
        error: Option<String>,
    }

    #[derive(Debug, Clone, Serialize)]
    struct ProductionPairRow {
        manifest_index: usize,
        id: String,
        path: String,
        audio_samples: usize,
        audio_seconds: f64,
        control: ProductionRunRow,
        candidate: ProductionRunRow,
        candidate_matches_control: bool,
    }

    #[derive(Debug, Serialize)]
    struct ProductionOutput {
        schema: &'static str,
        mode: &'static str,
        control_model_dir: String,
        candidate_model_dir: String,
        manifest: String,
        manifest_rows: usize,
        sample_rate_hz: u32,
        load_ms: f64,
        bias_installed: usize,
        candidate_sanity: Vec<[f32; 9]>,
        control_activation: serde_json::Value,
        candidate_activation: serde_json::Value,
        control_fused_compute: Option<String>,
        candidate_fused_compute: Option<String>,
        candidate_frontend_stats: Option<serde_json::Value>,
        warmup_control: ProductionRunRow,
        warmup_candidate: ProductionRunRow,
        rows_completed: usize,
        rows: Vec<ProductionPairRow>,
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

    fn process_cpu() -> (f64, f64) {
        unsafe {
            let mut r: libc::rusage = std::mem::zeroed();
            assert_eq!(libc::getrusage(libc::RUSAGE_SELF, &mut r), 0);
            (
                r.ru_utime.tv_sec as f64 + r.ru_utime.tv_usec as f64 / 1e6,
                r.ru_stime.tv_sec as f64 + r.ru_stime.tv_usec as f64 / 1e6,
            )
        }
    }
    fn configured_beam_width() -> usize {
        std::env::var("RIGHTKIT_ASR_BEAM_WIDTH")
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .map(|width| width.clamp(1, 8))
            .unwrap_or(2)
    }

    fn lookahead_counts(runtime: &AsrRuntime) -> [usize; 4] {
        runtime
            .frontend_lookahead_stats()
            .map(|s| {
                [
                    s.counters.requests,
                    s.counters.encoded,
                    s.counters.consumed,
                    s.counters.failures,
                ]
            })
            .unwrap_or([0; 4])
    }

    fn peak_rss_bytes() -> i64 {
        unsafe {
            let mut usage: libc::rusage = std::mem::zeroed();
            assert_eq!(libc::getrusage(libc::RUSAGE_SELF, &mut usage), 0);
            usage.ru_maxrss
        }
    }

    fn memory_gate() -> Result<
        (
            rightkit_dictation_inference::coreml_asr::MemoryGate,
            serde_json::Value,
        ),
        String,
    > {
        let physical = std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .map_err(|e| e.to_string())?;
        if !physical.status.success() {
            return Err("sysctl memory probe failed".into());
        }
        let physical_bytes: u64 = String::from_utf8_lossy(&physical.stdout)
            .trim()
            .parse()
            .map_err(|e| format!("physical memory: {e}"))?;
        let pressure = std::process::Command::new("/usr/bin/memory_pressure")
            .arg("-Q")
            .output()
            .map_err(|e| e.to_string())?;
        if !pressure.status.success() {
            return Err("memory_pressure probe failed".into());
        }
        let output = String::from_utf8_lossy(&pressure.stdout);
        let free_percent: u64 = output
            .lines()
            .find_map(|line| line.strip_prefix("System-wide memory free percentage: "))
            .ok_or("memory_pressure omitted availability estimate")?
            .trim()
            .trim_end_matches('%')
            .parse()
            .map_err(|e| format!("memory percentage: {e}"))?;
        if free_percent > 100 {
            return Err("invalid memory percentage".into());
        }
        let available_bytes = physical_bytes / 100 * free_percent;
        Ok((
            rightkit_dictation_inference::coreml_asr::MemoryGate {
                physical_memory_bytes: physical_bytes,
                available_memory_bytes: available_bytes,
            },
            serde_json::json!({"physical_bytes": physical_bytes, "available_estimate_bytes": available_bytes,
            "method": "sysctl hw.memsize * memory_pressure -Q system-wide free percentage (rounded down)",
            "memory_pressure_output": output}),
        ))
    }

    fn candidate_beam_width() -> Option<usize> {
        std::env::var("RIGHTKIT_COREML_BATCH_PROBE_BEAM_WIDTH")
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .map(|width| width.clamp(1, 8))
    }

    fn set_beam_width(runtime: &mut AsrRuntime, width: usize) {
        if let AsrRuntime::CoreMlParakeet(model) = runtime {
            model.set_beam_width(width);
        }
    }

    fn set_candidate_policy(batch: bool, duration_cap: Option<&str>, max_symbols: Option<&str>) {
        if batch {
            match duration_cap {
                Some(value) => std::env::set_var(DURATION_CAP_ENV, value),
                None => std::env::remove_var(DURATION_CAP_ENV),
            }
            match max_symbols {
                Some(value) => std::env::set_var(MAX_SYMBOLS_ENV, value),
                None => std::env::remove_var(MAX_SYMBOLS_ENV),
            }
        } else {
            std::env::remove_var(DURATION_CAP_ENV);
            std::env::remove_var(MAX_SYMBOLS_ENV);
        }
    }

    fn decode_once(
        runtime: &mut AsrRuntime,
        audio: &[f32],
        batch: bool,
        base_beam_width: usize,
        candidate_beam_width: Option<usize>,
        candidate_duration_cap: Option<&str>,
        candidate_max_symbols: Option<&str>,
    ) -> RunRow {
        for (source, target) in [
            (
                "RIGHTKIT_COREML_BATCH_PROBE_DISABLE_CACHE",
                "RIGHTKIT_ASR_DISABLE_PREDICTOR_CACHE",
            ),
            (
                "RIGHTKIT_COREML_BATCH_PROBE_PRUNING",
                "RIGHTKIT_COREML_PRUNING_PROBE",
            ),
            (
                "RIGHTKIT_COREML_BATCH_PROBE_ALIGNMENT",
                "RIGHTKIT_COREML_ARRAY_ALIGNMENT_PROBE",
            ),
            (
                "RIGHTKIT_COREML_BATCH_PROBE_PREFETCH",
                "RIGHTKIT_COREML_ARRAY_PREFETCH_PROBE",
            ),
        ] {
            if batch {
                if let Ok(value) = std::env::var(source) {
                    std::env::set_var(target, value);
                } else {
                    std::env::remove_var(target);
                }
            } else {
                std::env::remove_var(target);
            }
        }
        if let AsrRuntime::CoreMlParakeet(model) = runtime {
            let fused =
                batch && std::env::var("RIGHTKIT_COREML_BATCH_PROBE_FUSED").as_deref() == Ok("1");
            let max_rows = std::env::var("RIGHTKIT_COREML_FUSED_PROBE_MAX_ROWS")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(8);
            model
                .configure_evaluator_fused_probe(fused, max_rows)
                .expect("evaluator fused configuration");
            model.configure_evaluator_input_reuse(
                batch
                    && std::env::var("RIGHTKIT_COREML_BATCH_PROBE_INPUT_REUSE").as_deref()
                        == Ok("1"),
            );
            model.configure_evaluator_output_backings(
                batch
                    && std::env::var("RIGHTKIT_COREML_BATCH_PROBE_OUTPUT_BACKING").as_deref()
                        == Ok("1"),
            );
            model.configure_evaluator_frontend_lookahead(
                batch
                    && std::env::var("RIGHTKIT_COREML_BATCH_PROBE_LOOKAHEAD").as_deref() == Ok("1"),
            );
        }
        set_beam_width(
            runtime,
            if batch {
                candidate_beam_width.unwrap_or(base_beam_width)
            } else {
                base_beam_width
            },
        );
        set_candidate_policy(batch, candidate_duration_cap, candidate_max_symbols);
        if batch {
            std::env::set_var(
                CACHE_ENV,
                std::env::var("RIGHTKIT_COREML_BATCH_PROBE_MODE").unwrap_or_else(|_| "1".into()),
            );
        } else {
            std::env::remove_var(CACHE_ENV);
        }
        rightkit_dictation_inference::coreml_asr::take_decoder_probe_counts();
        rightkit_dictation_inference::coreml_asr::take_fused_probe_counts();
        rightkit_dictation_inference::coreml::Stage::take_output_backing_probe_counts();
        let lookahead_before = lookahead_counts(runtime);
        let cpu_before = process_cpu();
        let started = Instant::now();
        let result = runtime.transcribe_final_result_under_lease(audio);
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let cpu_after = process_cpu();
        let user_ms = (cpu_after.0 - cpu_before.0) * 1000.0;
        let kernel_ms = (cpu_after.1 - cpu_before.1) * 1000.0;
        let lookahead_after = lookahead_counts(runtime);
        let frontend_lookahead_counts =
            std::array::from_fn(|i| lookahead_after[i].saturating_sub(lookahead_before[i]));
        let runtime_counts = rightkit_dictation_inference::coreml_asr::take_decoder_probe_counts();
        let fused_counts = rightkit_dictation_inference::coreml_asr::take_fused_probe_counts();
        let backing =
            rightkit_dictation_inference::coreml::Stage::take_output_backing_probe_counts();
        let output_backing_counts = [
            backing.requested,
            backing.honored,
            backing.ignored,
            backing.failed,
        ];
        let unique_runtime_invocations = runtime_counts[2] + runtime_counts[3] - fused_counts[0];
        let logical_predictor_calls = runtime_counts[0];
        let logical_joint_calls = runtime_counts[1];
        let predictor_invocations = runtime_counts[2];
        let joint_invocations = runtime_counts[3];
        let predictor_cache_reuses = logical_joint_calls.saturating_sub(logical_predictor_calls);
        match result {
            Ok(value) => RunRow {
                batch,
                elapsed_ms,
                user_ms,
                kernel_ms,
                runtime_counts,
                fused_counts,
                output_backing_counts,
                frontend_lookahead_counts,
                unique_runtime_invocations,
                logical_predictor_calls,
                logical_joint_calls,
                predictor_invocations,
                joint_invocations,
                predictor_cache_reuses,
                text: Some(value.text),
                tokens: token_rows(&value.tokens),
                error: None,
            },
            Err(error) => RunRow {
                batch,
                elapsed_ms,
                user_ms,
                kernel_ms,
                runtime_counts,
                fused_counts,
                output_backing_counts,
                frontend_lookahead_counts,
                unique_runtime_invocations,
                logical_predictor_calls,
                logical_joint_calls,
                predictor_invocations,
                joint_invocations,
                predictor_cache_reuses,
                text: None,
                tokens: Vec::new(),
                error: Some(error),
            },
        }
    }

    fn same_transcript(left: &RunRow, right: &RunRow) -> bool {
        left.text == right.text && left.tokens == right.tokens && left.error == right.error
    }

    fn production_decode_once(runtime: &mut AsrRuntime, audio: &[f32]) -> ProductionRunRow {
        rightkit_dictation_inference::coreml_asr::take_decoder_probe_counts();
        rightkit_dictation_inference::coreml_asr::take_fused_probe_counts();
        rightkit_dictation_inference::coreml::Stage::take_output_backing_probe_counts();
        let lookahead_before = lookahead_counts(runtime);
        let cpu_before = process_cpu();
        let started = Instant::now();
        let result = runtime.transcribe_final_result_under_lease(audio);
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let cpu_after = process_cpu();
        let lookahead_after = lookahead_counts(runtime);
        let runtime_counts = rightkit_dictation_inference::coreml_asr::take_decoder_probe_counts();
        let fused_counts = rightkit_dictation_inference::coreml_asr::take_fused_probe_counts();
        let backing =
            rightkit_dictation_inference::coreml::Stage::take_output_backing_probe_counts();
        let output_backing_counts = [
            backing.requested,
            backing.honored,
            backing.ignored,
            backing.failed,
        ];
        let frontend_lookahead_counts =
            std::array::from_fn(|i| lookahead_after[i].saturating_sub(lookahead_before[i]));
        match result {
            Ok(value) => ProductionRunRow {
                elapsed_ms,
                user_ms: (cpu_after.0 - cpu_before.0) * 1_000.0,
                kernel_ms: (cpu_after.1 - cpu_before.1) * 1_000.0,
                runtime_counts,
                fused_counts,
                output_backing_counts,
                frontend_lookahead_counts,
                text: Some(value.text),
                tokens: token_rows(&value.tokens),
                error: None,
            },
            Err(error) => ProductionRunRow {
                elapsed_ms,
                user_ms: (cpu_after.0 - cpu_before.0) * 1_000.0,
                kernel_ms: (cpu_after.1 - cpu_before.1) * 1_000.0,
                runtime_counts,
                fused_counts,
                output_backing_counts,
                frontend_lookahead_counts,
                text: None,
                tokens: Vec::new(),
                error: Some(error),
            },
        }
    }

    fn same_production_transcript(left: &ProductionRunRow, right: &ProductionRunRow) -> bool {
        left.error.is_none()
            && right.error.is_none()
            && left.text == right.text
            && left.tokens == right.tokens
    }

    fn activation_json(model: &CoreMlParakeet) -> (serde_json::Value, Option<serde_json::Value>) {
        let activation = model.production_activation();
        let stats = model.frontend_lookahead_stats().map(|stats| {
            serde_json::json!({
                "requests": stats.counters.requests,
                "encoded": stats.counters.encoded,
                "consumed": stats.counters.consumed,
                "failures": stats.counters.failures,
                "estimated_peak_bytes": stats.estimated_peak_bytes,
                "max_memory_bytes": stats.max_memory_bytes,
                "physical_memory_bytes": stats.physical_memory_bytes,
                "available_memory_bytes": stats.available_memory_bytes,
                "worker_resident": stats.worker_resident,
            })
        });
        (
            serde_json::json!({
                "fused_loaded": activation.fused_loaded,
                "fused_max_rows": activation.fused_max_rows,
                "all_stage_output_backings": activation.all_stage_output_backings,
                "frontend_lookahead_admitted": activation.frontend_lookahead_admitted,
            }),
            stats,
        )
    }

    fn assert_production_probe_env_clean() -> Result<(), String> {
        let forbidden = std::env::vars()
            .map(|(name, _)| name)
            .filter(|name| {
                name.starts_with("RIGHTKIT_COREML")
                    || name == "RIGHTKIT_ASR_BEAM_WIDTH"
                    || name.starts_with("RIGHTKIT_ASR_DISABLE")
                    || matches!(
                        name.as_str(),
                        "RIGHTKIT_COREML_FUSED_PROBE_DIR"
                            | "RIGHTKIT_COREML_FUSED_PROBE_ENABLED"
                            | "RIGHTKIT_COREML_FUSED_PROBE_MAX_ROWS"
                            | "RIGHTKIT_COREML_FUSED_PROBE_PROFILE"
                            | "RIGHTKIT_COREML_OUTPUT_BACKING_PROBE"
                            | "RIGHTKIT_COREML_INPUT_REUSE_PROBE"
                            | "RIGHTKIT_COREML_FRONTEND_LOOKAHEAD"
                    )
            })
            .collect::<Vec<_>>();
        if forbidden.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "--production-pair forbids evaluator probe environment: {}",
                forbidden.join(", ")
            ))
        }
    }

    fn error_run(batch: bool, error: &str) -> RunRow {
        RunRow {
            batch,
            elapsed_ms: 0.0,
            user_ms: 0.0,
            kernel_ms: 0.0,
            runtime_counts: [0; 4],
            fused_counts: [0; 2],
            output_backing_counts: [0; 4],
            frontend_lookahead_counts: [0; 4],
            unique_runtime_invocations: 0,
            logical_predictor_calls: 0,
            logical_joint_calls: 0,
            predictor_invocations: 0,
            joint_invocations: 0,
            predictor_cache_reuses: 0,
            text: None,
            tokens: Vec::new(),
            error: Some(error.to_string()),
        }
    }

    fn write_snapshot(output_path: &Path, output: &impl Serialize) -> Result<(), String> {
        let file = File::create(output_path).map_err(|e| e.to_string())?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, output).map_err(|e| e.to_string())?;
        writer.write_all(b"\n").map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
        writer.get_ref().sync_data().map_err(|e| e.to_string())
    }

    fn parse_args(args: &[String]) -> Result<(PathBuf, PathBuf, PathBuf), String> {
        if args.len() != 3 {
            return Err(
                "usage: parakeet_hypothesis_batch <model-dir> <candidate-manifest.json> <output.json>"
                    .into(),
            );
        }
        Ok((
            PathBuf::from(&args[0]),
            PathBuf::from(&args[1]),
            PathBuf::from(&args[2]),
        ))
    }

    fn read_inputs(manifest_path: &Path) -> Result<Vec<InputRow>, String> {
        let manifest_text = fs::read_to_string(manifest_path).map_err(|e| e.to_string())?;
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
        Ok(inputs)
    }

    fn run_production_pair(args: &[String]) -> Result<(), String> {
        if args.len() != 4 && !(args.len() == 6 && args[4] == "--bias-file") {
            return Err(
                "usage: parakeet_hypothesis_batch --production-pair <split-model-dir> <candidate-model-dir> <manifest.json> <output.json> [--bias-file <phrases.json>]"
                    .into(),
            );
        }
        assert_production_probe_env_clean()?;
        let control_dir = PathBuf::from(&args[0]);
        let candidate_dir = PathBuf::from(&args[1]);
        let manifest_path = PathBuf::from(&args[2]);
        let output_path = PathBuf::from(&args[3]);
        if output_path.exists() {
            return Err(format!("output already exists: {}", output_path.display()));
        }
        let inputs = read_inputs(&manifest_path)?;
        if let Some(parent) = output_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::env::set_var("RIGHTKIT_ASR_PREDICTOR_DIAGNOSTICS", "1");
        eprintln!(
            "parakeet_hypothesis_batch production-pair manifest_rows={} control={} candidate={}",
            inputs.len(),
            control_dir.display(),
            candidate_dir.display()
        );
        let load_started = Instant::now();
        let mut control = AsrRuntime::CoreMlParakeet(CoreMlParakeet::load(&control_dir)?);
        let mut candidate = AsrRuntime::CoreMlParakeet(CoreMlParakeet::load(&candidate_dir)?);
        let load_ms = load_started.elapsed().as_secs_f64() * 1_000.0;
        let bias_installed = if args.len() == 6 {
            let phrases: Vec<String> = serde_json::from_str(
                &fs::read_to_string(&args[5]).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            let AsrRuntime::CoreMlParakeet(control_model) = &mut control else {
                unreachable!()
            };
            let control_count = control_model.set_context_bias_phrases(phrases.iter(), 1.0);
            let AsrRuntime::CoreMlParakeet(candidate_model) = &mut candidate else {
                unreachable!()
            };
            let candidate_count = candidate_model.set_context_bias_phrases(phrases.iter(), 1.0);
            if candidate_count != control_count {
                return Err("production bias installation differs".into());
            }
            candidate_count
        } else {
            0
        };
        let candidate_sanity = match &candidate {
            AsrRuntime::CoreMlParakeet(model) => model
                .fused_probe_sanity()?
                .ok_or("production graph sanity missing")?,
            _ => unreachable!(),
        };
        let (control_activation, _) = match &control {
            AsrRuntime::CoreMlParakeet(model) => activation_json(model),
            _ => unreachable!(),
        };
        let (candidate_activation, candidate_frontend_stats) = match &candidate {
            AsrRuntime::CoreMlParakeet(model) => activation_json(model),
            _ => unreachable!(),
        };
        let candidate_state = match &candidate {
            AsrRuntime::CoreMlParakeet(model) => model.production_activation(),
            _ => unreachable!(),
        };
        if !candidate_state.fused_loaded
            || candidate_state.fused_max_rows != 2
            || !candidate_state.all_stage_output_backings
            || !candidate_state.frontend_lookahead_admitted
        {
            return Err(format!(
                "candidate production activation incomplete: {candidate_activation}"
            ));
        }
        let control_state = match &control {
            AsrRuntime::CoreMlParakeet(model) => model.production_activation(),
            _ => unreachable!(),
        };
        if control_state.fused_loaded {
            return Err("control model unexpectedly loaded production fused graph".into());
        }
        let control_fused_compute = match &control {
            AsrRuntime::CoreMlParakeet(model) => {
                model.fused_probe_compute_profile().map(str::to_owned)
            }
            _ => None,
        };
        let candidate_fused_compute = match &candidate {
            AsrRuntime::CoreMlParakeet(model) => {
                model.fused_probe_compute_profile().map(str::to_owned)
            }
            _ => None,
        };
        let warm_audio = read_wav(Path::new(&inputs[0].path))?;
        let warmup_control = production_decode_once(&mut control, &warm_audio);
        let warmup_candidate = production_decode_once(&mut candidate, &warm_audio);
        if let Some(error) = warmup_control
            .error
            .as_ref()
            .or(warmup_candidate.error.as_ref())
        {
            return Err(format!("production warmup failed: {error}"));
        }
        let mut output = ProductionOutput {
            schema: "heardright.parakeet_production_pair.v1",
            mode: "production_pair",
            control_model_dir: control_dir.display().to_string(),
            candidate_model_dir: candidate_dir.display().to_string(),
            manifest: manifest_path.display().to_string(),
            manifest_rows: inputs.len(),
            sample_rate_hz: SAMPLE_RATE,
            load_ms,
            bias_installed,
            candidate_sanity,
            control_activation,
            candidate_activation,
            control_fused_compute,
            candidate_fused_compute,
            candidate_frontend_stats,
            warmup_control,
            warmup_candidate,
            rows_completed: 0,
            rows: Vec::with_capacity(inputs.len()),
        };
        write_snapshot(&output_path, &output)?;
        for (manifest_index, input) in inputs.iter().enumerate() {
            let audio = read_wav(Path::new(&input.path))?;
            let control_run = production_decode_once(&mut control, &audio);
            let candidate_run = production_decode_once(&mut candidate, &audio);
            if control_run.error.is_some() || candidate_run.error.is_some() {
                return Err(format!(
                    "production clip {} failed: control={:?} candidate={:?}",
                    input.id, control_run.error, candidate_run.error
                ));
            }
            let candidate_matches_control =
                same_production_transcript(&control_run, &candidate_run);
            output.rows.push(ProductionPairRow {
                manifest_index,
                id: input.id.clone(),
                path: input.path.clone(),
                audio_samples: audio.len(),
                audio_seconds: audio.len() as f64 / SAMPLE_RATE as f64,
                control: control_run,
                candidate: candidate_run,
                candidate_matches_control,
            });
            output.candidate_frontend_stats = match &candidate {
                AsrRuntime::CoreMlParakeet(model) => activation_json(model).1,
                _ => None,
            };
            output.rows_completed += 1;
            write_snapshot(&output_path, &output)?;
        }
        eprintln!(
            "parakeet_hypothesis_batch production-pair complete rows={} load_ms={load_ms:.1} output={}",
            output.rows_completed,
            output_path.display()
        );
        Ok(())
    }

    fn run_production_arm(args: &[String]) -> Result<(), String> {
        if args.len() != 4 && !(args.len() == 6 && args[4] == "--bias-file") {
            return Err("usage: --production-arm <control|candidate> <model-dir> <manifest> <output> [--bias-file <phrases>]".into());
        }
        assert_production_probe_env_clean()?;
        let is_candidate = match args[0].as_str() {
            "candidate" => true,
            "control" => false,
            _ => return Err("production arm must be control or candidate".into()),
        };
        let model_dir = PathBuf::from(&args[1]);
        let manifest = PathBuf::from(&args[2]);
        let output_path = PathBuf::from(&args[3]);
        if output_path.exists() {
            return Err("production output already exists".into());
        }
        let inputs = read_inputs(&manifest)?;
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::env::set_var("RIGHTKIT_ASR_PREDICTOR_DIAGNOSTICS", "1");
        let started = Instant::now();
        let mut model = CoreMlParakeet::load(&model_dir)?;
        let load_ms = started.elapsed().as_secs_f64() * 1000.0;
        let bias_installed = if args.len() == 6 {
            let phrases: Vec<String> =
                serde_json::from_str(&fs::read_to_string(&args[5]).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            model.set_context_bias_phrases(phrases.iter(), 1.0)
        } else {
            0
        };
        let state = model.production_activation();
        let (activation, frontend_stats) = activation_json(&model);
        if is_candidate
            && (!state.fused_loaded
                || state.fused_max_rows != 2
                || !state.all_stage_output_backings
                || !state.frontend_lookahead_admitted)
        {
            return Err(format!(
                "production candidate activation incomplete: {activation}"
            ));
        }
        if !is_candidate && state.fused_loaded {
            return Err("split control unexpectedly fused".into());
        }
        let sanity = if is_candidate {
            model.fused_probe_sanity()?
        } else {
            None
        };
        let fused_compute = model.fused_probe_compute_profile().map(str::to_owned);
        let (encoder_compute, decoder_compute) = model.configured_compute_route();
        let (encoder_compute, decoder_compute) =
            (encoder_compute.to_owned(), decoder_compute.to_owned());
        let mut runtime = AsrRuntime::CoreMlParakeet(model);
        let warmup = production_decode_once(&mut runtime, &read_wav(Path::new(&inputs[0].path))?);
        if warmup.error.is_some() {
            return Err(format!("production warmup failed: {:?}", warmup.error));
        }
        // Warm separately loaded frontend using actual multi-window speech.
        // Both arms use identical audio; exclude setup from measured requests.
        let long_audio = inputs
            .iter()
            .find_map(|row| {
                read_wav(Path::new(&row.path))
                    .ok()
                    .filter(|audio| audio.len() > 15 * SAMPLE_RATE as usize)
            })
            .ok_or("production confirmation needs a multi-window warmup clip")?;
        let frontend_warmup = production_decode_once(&mut runtime, &long_audio);
        if let Some(error) = &frontend_warmup.error {
            return Err(format!("production frontend warmup failed: {error}"));
        }
        let counts = frontend_warmup.frontend_lookahead_counts;
        if is_candidate
            && (counts[0] == 0
                || counts[0] != counts[1]
                || counts[1] != counts[2]
                || counts[3] != 0)
        {
            return Err("production frontend warmup did not consume requested windows".into());
        }
        let mut output = serde_json::json!({
            "schema": "heardright.parakeet_production_arm.v1", "arm": args[0],
            "model_dir": model_dir, "manifest": manifest, "manifest_rows": inputs.len(),
            "load_ms": load_ms, "bias_installed": bias_installed, "activation": activation,
            "frontend_stats": frontend_stats, "sanity": sanity, "fused_compute": fused_compute,
            "encoder_compute": encoder_compute, "decoder_compute": decoder_compute,
            "warmup": warmup, "frontend_warmup": frontend_warmup, "rows_completed": 0, "rows": [],
        });
        write_snapshot(&output_path, &output)?;
        for (index, input) in inputs.iter().enumerate() {
            let audio = read_wav(Path::new(&input.path))?;
            let run = production_decode_once(&mut runtime, &audio);
            if run.error.is_some() {
                return Err(format!(
                    "production clip {} failed: {:?}",
                    input.id, run.error
                ));
            }
            output["rows"].as_array_mut().unwrap().push(serde_json::json!({
                "manifest_index": index, "id": input.id, "audio_samples": audio.len(), "run": run,
            }));
            output["rows_completed"] = serde_json::json!(index + 1);
            if let AsrRuntime::CoreMlParakeet(model) = &runtime {
                output["frontend_stats"] = serde_json::json!(activation_json(model).1);
            }
            write_snapshot(&output_path, &output)?;
        }
        eprintln!("production arm {} complete rows={}", args[0], inputs.len());
        Ok(())
    }

    pub fn run() -> Result<(), String> {
        let args: Vec<_> = std::env::args().skip(1).collect();
        if args.first().map(String::as_str) == Some("--production-arm") {
            return run_production_arm(&args[1..]);
        }
        if args.first().map(String::as_str) == Some("--production-pair") {
            return run_production_pair(&args[1..]);
        }
        let (model_dir, manifest_path, output_path) = parse_args(&args)?;
        let paired_only =
            std::env::var("RIGHTKIT_COREML_BATCH_PROBE_PAIRED_ONLY").as_deref() == Ok("1");
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
            "parakeet_hypothesis_batch start manifest_rows={} model={}",
            inputs.len(),
            model_dir.display()
        );
        let load_started = Instant::now();
        std::env::set_var("RIGHTKIT_ASR_PREDICTOR_DIAGNOSTICS", "1");
        let mut model = CoreMlParakeet::load(&model_dir)?;
        if std::env::var_os("RIGHTKIT_COREML_FUSED_PROBE_DIR").is_some() {
            let max_rows = std::env::var("RIGHTKIT_COREML_FUSED_PROBE_MAX_ROWS")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(8);
            model.configure_evaluator_fused_probe(false, max_rows)?;
        }
        let bias_file = std::env::var("RIGHTKIT_COREML_BATCH_PROBE_BIAS_FILE").ok();
        let bias_score = bias_file.as_ref().map(|_| {
            std::env::var("RIGHTKIT_COREML_BATCH_PROBE_BIAS_SCORE")
                .ok()
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or(1.0)
        });
        let bias_installed = if let Some(path) = &bias_file {
            let phrases: Vec<String> =
                serde_json::from_str(&fs::read_to_string(path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            model.set_context_bias_phrases(phrases.iter(), bias_score.unwrap())
        } else {
            0
        };

        let base_beam_width = configured_beam_width();
        let candidate_beam_width = candidate_beam_width();
        let candidate_duration_cap = std::env::var("RIGHTKIT_COREML_BATCH_PROBE_DURATION_CAP").ok();
        let candidate_max_symbols = std::env::var("RIGHTKIT_COREML_BATCH_PROBE_MAX_SYMBOLS").ok();
        let (encoder_compute, decoder_compute) = model.configured_compute_route();
        let encoder_compute = encoder_compute.to_owned();
        let decoder_compute = decoder_compute.to_owned();
        let fused_compute = model.fused_probe_compute_profile().map(str::to_owned);
        let sanity = model.hypothesis_batch_sanity()?;
        let fused_sanity = model.fused_probe_sanity()?;
        let aligned_fused_sanity =
            if std::env::var("RIGHTKIT_COREML_BATCH_PROBE_ALIGNMENT").as_deref() == Ok("1") {
                std::env::set_var("RIGHTKIT_COREML_ARRAY_ALIGNMENT_PROBE", "1");
                let result = model.fused_probe_sanity();
                std::env::remove_var("RIGHTKIT_COREML_ARRAY_ALIGNMENT_PROBE");
                let result = result?;
                if result != fused_sanity {
                    return Err("aligned arrays changed independent-state sanity outputs".into());
                }
                result
            } else {
                None
            };
        let mut fused_buffer_sanity = None;
        let buffer_reuse_sanity = if std::env::var("RIGHTKIT_COREML_BATCH_PROBE_INPUT_REUSE")
            .as_deref()
            == Ok("1")
            || std::env::var("RIGHTKIT_COREML_BATCH_PROBE_OUTPUT_BACKING").as_deref() == Ok("1")
        {
            model.configure_evaluator_input_reuse(
                std::env::var("RIGHTKIT_COREML_BATCH_PROBE_INPUT_REUSE").as_deref() == Ok("1"),
            );
            model.configure_evaluator_output_backings(
                std::env::var("RIGHTKIT_COREML_BATCH_PROBE_OUTPUT_BACKING").as_deref() == Ok("1"),
            );
            let result = model.hypothesis_batch_sanity();
            let fused_result = model.fused_probe_sanity();
            model.configure_evaluator_input_reuse(false);
            model.configure_evaluator_output_backings(false);
            fused_buffer_sanity = fused_result?;
            if fused_buffer_sanity != fused_sanity {
                return Err("fused buffer reuse changed independent-state sanity outputs".into());
            }
            Some(result?)
        } else {
            None
        };
        eprintln!("RIGHTKIT_BATCH_SANITY_MAX_ABS={sanity:?}");
        let frontend_lookahead_preparation = if std::env::var(
            "RIGHTKIT_COREML_BATCH_PROBE_LOOKAHEAD",
        )
        .as_deref()
            == Ok("1")
        {
            let (gate, facts) = memory_gate()?;
            let rss_before = peak_rss_bytes();
            let output_backings =
                std::env::var("RIGHTKIT_COREML_BATCH_PROBE_OUTPUT_BACKING").as_deref() == Ok("1");
            let prepared = model.prepare_frontend_lookahead_evaluator(gate, output_backings);
            if !prepared? {
                return Err("lookahead candidate did not prepare worker".into());
            }
            let stats = model
                .frontend_lookahead_stats()
                .ok_or("prepared worker omitted stats")?;
            Some(
                serde_json::json!({"memory": facts, "estimated_transient_peak_bytes": stats.estimated_peak_bytes,
                "transient_budget_bytes": stats.max_memory_bytes, "worker_resident": stats.worker_resident,
                "process_peak_rss_bytes_before_prepare": rss_before, "process_peak_rss_bytes_after_prepare": peak_rss_bytes()}),
            )
        } else {
            None
        };
        let load_ms = load_started.elapsed().as_secs_f64() * 1_000.0;
        let mut runtime = AsrRuntime::CoreMlParakeet(model);
        let mut run_arm = |audio: &[f32], batch: bool| {
            decode_once(
                &mut runtime,
                audio,
                batch,
                base_beam_width,
                candidate_beam_width,
                candidate_duration_cap.as_deref(),
                candidate_max_symbols.as_deref(),
            )
        };

        // Exclude model/ANE setup from per-clip timing. Warm one real manifest
        // clip in each arm; silence can short-circuit a different path.
        let warm_audio = read_wav(Path::new(&inputs[0].path))?;
        let warm_started = Instant::now();
        let mut warmup_calls = 0;
        let mut frontend_warmup_counts = [0; 4];
        for batch in [false, true, false] {
            let result = run_arm(&warm_audio, batch);
            if let Some(error) = result.error {
                return Err(format!("warmup failed: {error}"));
            }
            warmup_calls += 1;
        }
        // A short first clip never invokes the separately loaded frontend.
        // Warm one real multi-window clip before timing a lookahead candidate.
        if std::env::var("RIGHTKIT_COREML_BATCH_PROBE_LOOKAHEAD").as_deref() == Ok("1") {
            let long_audio = inputs
                .iter()
                .find_map(|row| {
                    read_wav(Path::new(&row.path))
                        .ok()
                        .filter(|audio| audio.len() > 15 * SAMPLE_RATE as usize)
                })
                .ok_or("lookahead needs a multi-window warmup clip")?;
            for batch in [false, true, false] {
                let result = run_arm(&long_audio, batch);
                if let Some(error) = result.error {
                    return Err(format!("lookahead warmup failed: {error}"));
                }
                if batch {
                    frontend_warmup_counts = result.frontend_lookahead_counts;
                }
                warmup_calls += 1;
            }
            if frontend_warmup_counts[0] == 0
                || frontend_warmup_counts[0] != frontend_warmup_counts[1]
                || frontend_warmup_counts[1] != frontend_warmup_counts[2]
                || frontend_warmup_counts[3] != 0
            {
                return Err("lookahead warmup did not consume every requested window".into());
            }
        }
        let warmup_ms = warm_started.elapsed().as_secs_f64() * 1_000.0;

        let mut output = Output {
            schema: "heardright.parakeet_hypothesis_batch.v5",
            paired_only,
            model_dir: model_dir.display().to_string(),
            manifest: manifest_path.display().to_string(),
            manifest_rows: inputs.len(),
            sample_rate_hz: SAMPLE_RATE,
            load_ms,
            sanity_max_abs: sanity,
            fused_sanity,
            fused_buffer_sanity,
            aligned_fused_sanity,
            frontend_lookahead_preparation,
            buffer_reuse_sanity,
            bias_file,
            bias_score,
            bias_installed,
            beam_width: std::env::var("RIGHTKIT_ASR_BEAM_WIDTH").unwrap_or_else(|_| "2".into()),
            candidate_beam_width,
            candidate_duration_cap: candidate_duration_cap.clone(),
            candidate_max_symbols: candidate_max_symbols.clone(),
            batch_mode: std::env::var("RIGHTKIT_COREML_BATCH_PROBE_MODE")
                .unwrap_or_else(|_| "1".into()),
            batch_max_rows: std::env::var("RIGHTKIT_COREML_HYP_BATCH_MAX_ROWS")
                .unwrap_or_else(|_| "unlimited".into()),
            encoder_compute,
            decoder_compute,
            fused_compute,
            warmup_ms,
            warmup_calls,
            frontend_warmup_counts,
            rows_completed: 0,
            rows: Vec::with_capacity(inputs.len()),
        };
        write_snapshot(&output_path, &output)?;

        for (manifest_index, input) in inputs.iter().enumerate() {
            eprintln!(
                "parakeet_hypothesis_batch clip {}/{} id={}",
                manifest_index + 1,
                inputs.len(),
                input.id
            );
            let audio = match read_wav(Path::new(&input.path)) {
                Ok(audio) => audio,
                Err(error) => {
                    let current = error_run(false, &error);
                    let batched = error_run(true, &error);
                    let current_repeat = (!paired_only).then(|| error_run(false, &error));
                    output.rows.push(ParityRow {
                        manifest_index,
                        id: input.id.clone(),
                        path: input.path.clone(),
                        audio_samples: 0,
                        audio_seconds: 0.0,
                        current,
                        batched,
                        current_repeat,
                        batched_matches_current: false,
                        current_repeat_matches_current: (!paired_only).then_some(false),
                    });
                    output.rows_completed += 1;
                    write_snapshot(&output_path, &output)?;
                    std::env::remove_var(CACHE_ENV);
                    std::env::remove_var(DURATION_CAP_ENV);
                    std::env::remove_var(MAX_SYMBOLS_ENV);
                    return Err(format!("clip {} audio read failed: {error}", input.id));
                }
            };
            let audio_seconds = audio.len() as f64 / SAMPLE_RATE as f64;
            let (current, batched, current_repeat) = if paired_only {
                let (current, batched) = if manifest_index % 2 == 0 {
                    let a = run_arm(&audio, false);
                    let b = run_arm(&audio, true);
                    (a, b)
                } else {
                    let b = run_arm(&audio, true);
                    let a = run_arm(&audio, false);
                    (a, b)
                };
                (current, batched, None)
            } else {
                match manifest_index % 3 {
                    0 => {
                        let a = run_arm(&audio, false);
                        let b = run_arm(&audio, true);
                        let c = run_arm(&audio, false);
                        (a, b, Some(c))
                    }
                    1 => {
                        let b = run_arm(&audio, true);
                        let a = run_arm(&audio, false);
                        let c = run_arm(&audio, false);
                        (a, b, Some(c))
                    }
                    _ => {
                        let a = run_arm(&audio, false);
                        let c = run_arm(&audio, false);
                        let b = run_arm(&audio, true);
                        (a, b, Some(c))
                    }
                }
            };
            let batched_matches_current = same_transcript(&batched, &current);
            let current_repeat_matches_current = current_repeat
                .as_ref()
                .map(|repeat| same_transcript(repeat, &current));
            let decode_error = [&current, &batched]
                .iter()
                .find_map(|run| run.error.clone())
                .or_else(|| current_repeat.as_ref().and_then(|run| run.error.clone()));
            output.rows.push(ParityRow {
                manifest_index,
                id: input.id.clone(),
                path: input.path.clone(),
                audio_samples: audio.len(),
                audio_seconds,
                current,
                batched,
                current_repeat,
                batched_matches_current,
                current_repeat_matches_current,
            });
            output.rows_completed += 1;
            write_snapshot(&output_path, &output)?;
            if let Some(error) = decode_error {
                std::env::remove_var(CACHE_ENV);
                std::env::remove_var(DURATION_CAP_ENV);
                std::env::remove_var(MAX_SYMBOLS_ENV);
                return Err(format!("clip {} decode failed: {error}", input.id));
            }
        }
        std::env::remove_var(CACHE_ENV);
        std::env::remove_var(DURATION_CAP_ENV);
        std::env::remove_var(MAX_SYMBOLS_ENV);
        eprintln!(
            "parakeet_hypothesis_batch complete rows={} output={}",
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
