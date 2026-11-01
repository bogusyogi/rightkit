//! Bounded full-corpus comparison of Windows production decode duration caps.
//!
//! One loaded ONNX runtime and model are reused for every arm. Each canonical
//! clip runs at caps 1, 2, 4, plus a cap-2 repeat; arm order rotates by clip.
//! Results are flushed one clip at a time so interrupted runs retain evidence.
//!
//! Windows only. The other-platform entry point is empty to keep this example
//! harmless in non-Windows example builds.
//!
//! Usage:
//!   windows_duration_cap_parity <model-dir> <wav-dir> <output.jsonl> [dml|cpu] [row-limit]

#[cfg(not(target_os = "windows"))]
fn main() {}

#[cfg(target_os = "windows")]
mod windows {
    use serde::Serialize;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    #[derive(Serialize)]
    struct TokenRow {
        text: String,
        start: f32,
        end: f32,
    }

    #[derive(Serialize)]
    struct ArmRow {
        cap: usize,
        repeat: bool,
        elapsed_ms: f64,
        text: String,
        tokens: Vec<TokenRow>,
    }

    #[derive(Serialize)]
    struct ClipRow {
        id: String,
        wav_path: String,
        audio_seconds: f64,
        order: Vec<String>,
        arms: Vec<ArmRow>,
        errors: Vec<String>,
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

    fn arm_name(cap: usize, repeat: bool) -> String {
        if repeat {
            format!("cap{cap}_repeat")
        } else {
            format!("cap{cap}")
        }
    }

    fn decode(
        runtime: &mut rightkit_dictation_inference::asr::AsrRuntime,
        audio: &[f32],
        cap: usize,
        repeat: bool,
        id: &str,
    ) -> Result<ArmRow, String> {
        // Rust 2021 environment updates are safe; the harness is strictly serial.
        std::env::set_var("RIGHTKIT_TDT_DURATION_CAP", cap.to_string());
        let label = arm_name(cap, repeat);
        eprintln!("RIGHTKIT_DURATION_CAP_BEGIN id={id} arm={label}");
        let started = Instant::now();
        let result = runtime.transcribe_final_result_under_lease(audio);
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let result = result.map_err(|e| format!("id={id} arm={label}: {e}"))?;
        Ok(ArmRow {
            cap,
            repeat,
            elapsed_ms,
            text: result.text,
            tokens: result
                .tokens
                .into_iter()
                .map(|t| TokenRow {
                    text: t.text,
                    start: t.start,
                    end: t.end,
                })
                .collect(),
        })
    }

    fn execute() -> Result<(), String> {
        let args: Vec<String> = std::env::args().collect();
        if args.len() < 4 {
            return Err("usage: windows_duration_cap_parity <model-dir> <wav-dir> <output.jsonl> [dml|cpu] [row-limit]".into());
        }
        std::env::set_var("RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT", "fused");
        std::env::remove_var("RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT_DIR");
        std::env::set_var("RIGHTKIT_ASR_BACKEND", "parakeet-tdt");
        std::env::set_var("RIGHTKIT_DICTATION_LANG", "auto");
        std::env::set_var("RIGHTKIT_ASR_BEAM_WIDTH", "2");
        std::env::set_var("RIGHTKIT_ASR_BEAM_EMIT_REWARD", "0.10");
        std::env::set_var("RIGHTKIT_TDT_PRUNING", "global");
        std::env::remove_var("RIGHTKIT_ONNX_CPU_THREADS_EXPERIMENT");
        std::env::set_var("RIGHTKIT_PREDICTOR_BENCH", "1");

        let provider = match args.get(4).map(String::as_str) {
            Some("cpu") => rightkit_dictation_inference::asr::AsrEp::Cpu,
            _ => rightkit_dictation_inference::asr::AsrEp::Dml,
        };
        eprintln!("RIGHTKIT_DURATION_CAP_CONFIG model={} wav_dir={} provider={} backend=parakeet-tdt language=auto beam_width=2 emission_reward=0.10 pruning=global predictor=fused cpu_threads_experiment=cleared ort_dylib={}",
            args[1], args[2], if matches!(provider, rightkit_dictation_inference::asr::AsrEp::Cpu) { "cpu" } else { "dml" },
            std::env::var("ORT_DYLIB_PATH").unwrap_or_else(|_| "unset".into()));
        let mut runtime =
            rightkit_dictation_inference::asr::AsrRuntime::load(Path::new(&args[1]), provider)
                .map_err(|e| format!("load model: {e}"))?;

        let mut wavs: Vec<PathBuf> = std::fs::read_dir(&args[2])
            .map_err(|e| format!("read WAV dir: {e}"))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
            })
            .collect();
        wavs.sort();
        if wavs.len() != 224 {
            return Err(format!("expected 224 canonical WAVs, found {}", wavs.len()));
        }
        let distinct: std::collections::HashSet<_> =
            wavs.iter().filter_map(|p| p.file_stem()).collect();
        if distinct.len() != 224 {
            return Err(format!(
                "expected 224 unique canonical IDs, found {}",
                distinct.len()
            ));
        }
        let row_limit = args
            .get(5)
            .map(|s| {
                s.parse::<usize>()
                    .map_err(|e| format!("invalid row limit {s}: {e}"))
            })
            .transpose()?
            .unwrap_or(224);
        if row_limit == 0 || row_limit > 224 {
            return Err(format!(
                "row limit must be between 1 and 224, got {row_limit}"
            ));
        }
        let out_path = Path::new(&args[3]);
        if out_path.exists() {
            return Err(format!(
                "refusing to overwrite existing evidence: {}",
                out_path.display()
            ));
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create output dir: {e}"))?;
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(out_path)
            .map_err(|e| format!("create output: {e}"))?;
        let mut out = std::io::BufWriter::new(file);

        // Warm each cap independently using the first canonical clip; discard outputs and timings.
        let warm_audio = load_wav(&wavs[0])?;
        for cap in [1, 2, 4] {
            let _ = decode(&mut runtime, &warm_audio, cap, false, "__warmup__")?;
        }
        eprintln!("RIGHTKIT_DURATION_CAP_WARMED caps=1,2,4 provider={} beam_width={} predictor_experiment=fused", if matches!(provider, rightkit_dictation_inference::asr::AsrEp::Cpu) { "cpu" } else { "dml" }, std::env::var("RIGHTKIT_ASR_BEAM_WIDTH").unwrap_or_else(|_| "2 (default)".into()));

        for (index, wav) in wavs.iter().take(row_limit).enumerate() {
            let audio = load_wav(wav)?;
            if audio.is_empty() {
                return Err(format!("empty audio: {}", wav.display()));
            }
            let seconds = audio.len() as f64 / 16_000.0;
            let id = wav
                .file_stem()
                .ok_or("WAV has no filename")?
                .to_string_lossy()
                .into_owned();
            let base = [(1usize, false), (2, false), (4, false), (2, true)];
            let order: Vec<(usize, bool)> = (0..4)
                .map(|offset| base[(index + offset) % base.len()])
                .collect();
            let mut arms = Vec::with_capacity(4);
            for &(cap, repeat) in &order {
                match decode(&mut runtime, &audio, cap, repeat, &id) {
                    Ok(row) => arms.push(row),
                    Err(error) => {
                        let partial = ClipRow {
                            id: id.clone(),
                            wav_path: wav.display().to_string(),
                            audio_seconds: seconds,
                            order: order.iter().map(|(c, r)| arm_name(*c, *r)).collect(),
                            arms,
                            errors: vec![error.clone()],
                        };
                        serde_json::to_writer(&mut out, &partial).map_err(|e| e.to_string())?;
                        out.write_all(b"\n")
                            .and_then(|_| out.flush())
                            .map_err(|e| e.to_string())?;
                        return Err(error);
                    }
                }
            }
            let row = ClipRow {
                id,
                wav_path: wav.display().to_string(),
                audio_seconds: seconds,
                order: order.iter().map(|(c, r)| arm_name(*c, *r)).collect(),
                arms,
                errors: Vec::new(),
            };
            serde_json::to_writer(&mut out, &row).map_err(|e| e.to_string())?;
            out.write_all(b"\n")
                .and_then(|_| out.flush())
                .map_err(|e| e.to_string())?;
            eprintln!(
                "RIGHTKIT_DURATION_CAP_PROGRESS completed={}/{}",
                index + 1,
                row_limit
            );
        }
        eprintln!(
            "RIGHTKIT_DURATION_CAP_COMPLETE clips={} output={}",
            row_limit,
            out_path.display()
        );
        Ok(())
    }

    pub fn main() {
        if let Err(error) = execute() {
            eprintln!("windows_duration_cap_parity: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "windows")]
fn main() {
    windows::main();
}
