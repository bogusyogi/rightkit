// Native the app control runner for the Open-ASR bakeoff ("Fresh
// The app controls" lane, docs/plans/2026-07-17-open-asr-top-model-dispatch-run-packet.md).
//
// Runs the SAME shipped engine code production uses — not a proxy, not a
// re-implementation — over the SAME already-conditioned candidate WAVs every
// external model lane scores against
// (scripts/open-asr-bakeoff/canonical-eval/candidate_audio/*.wav, produced once by
// condition-corpus.mjs via rightkit_dictation_core::audio_conditioning::condition_for_asr,
// policy `asr_simple_gain_hpf`):
//
//   - Unified / TDT: rightkit_dictation_engine::asr::{AsrEp, AsrRuntime} ->
//     AsrRuntime::load() + AsrRuntime::transcribe_result(), which internally
//     runs the exact production `transcribe_padded_window` policy (15s window,
//     2.24s trailing-silence search, timestamp commit, 16-char overlap trim —
//     see rightkit-dictation-inference/src/asr/runtime.rs).
//   - Whisper: rightkit_dictation_engine::whisper_win::WhisperWin -> WhisperWin::load()
//     + transcribe_lang(), the exact struct AsrRuntime::WhisperWin shells out
//     to in production (rightkit-dictation-inference/src/whisper_win.rs).
//
// This closes the gap `run-controls.mjs` (JS wiring/dry-run harness) documented
// as `blocking_gap`: "No Windows-native runner reproducing
// transcribe_padded_window was found in scripts/."
//
// -- Conditioning invariant (verified 2026-07-17, do not re-derive) --------
// Conditioning happens exactly ONCE, at the worker boundary
// (rightkit-dictation-runtime/src/worker/probe_decisions.rs:
// `condition_for_asr(buffer, SAMPLE_RATE, &audio_policy)` runs BEFORE the
// worker's `transcribe` closure is invoked). `condition_for_asr` is NOT called
// anywhere inside rightkit-dictation-inference/src/asr/runtime.rs (`transcribe_result`,
// `transcribe_padded_window`) or whisper_win.rs's `transcribe_lang` — grepped
// across rightkit-dictation-engine/heardright_core, the only call sites are the worker
// boundary, the `cond_check`/`buffered_tdt_partials` examples, and the
// `condition_audio` bin. whisper_win.rs's own doc comment confirms it: "already
// `condition_for_asr`-processed by the shared worker pipeline — this engine
// does NOT re-condition." Since candidate_audio/*.wav is already the
// conditioned output of condition-corpus.mjs, feeding it straight into
// `AsrRuntime::transcribe_result` / `WhisperWin::transcribe_lang` below matches
// production's single-conditioning invariant exactly. `env.json` records
// `reconditioned: false` to make this explicit and falsifiable.
//
// -- Usage (from tauri-app-next) -------------------------------------------
//   cargo build --release --manifest-path rightkit-dictation-engine/Cargo.toml --example openasr_controls
//   cargo run --release --manifest-path rightkit-dictation-engine/Cargo.toml --example openasr_controls -- --engine unified
//   cargo run --release --manifest-path rightkit-dictation-engine/Cargo.toml --example openasr_controls -- --engine tdt
//   cargo run --release --manifest-path rightkit-dictation-engine/Cargo.toml --example openasr_controls -- --engine whisper
//
// Flags:
//   --engine <unified|tdt|whisper>   required
//   --manifest <path>                default scripts/open-asr-bakeoff/canonical-eval/candidate_manifest.json
//   --models-root <path>             default <repo>/model_registry/final/asr
//   --out-dir <path>                 default scripts/open-asr-bakeoff/out/control-<engine>
//   --limit <n>                      optional row cap (smoke runs)
//   --ep <auto|cpu|dml|cuda>         Parakeet execution provider; default auto
//   --scheduled-window-seconds <n>   scheduled-static window; default 15, minimum 3
//   --rolling-static-commit-seconds <n>  15s static context, commit every n seconds
//   --rolling-static-quiet-max-seconds <n>  after commit minimum, seek quiet through n seconds
//   --dynamic-full-sequence          decode each complete clip as one dynamic sequence
//
// Output (per row, flushed after every clip so a killed process leaves usable
// partial progress):
//   <out-dir>/hypotheses.jsonl  {id, hypothesis, decode_s, status}
//     status: "ok" (non-empty decode) | "empty" (decode ok, blank text) |
//             "unsupported" (decode returned Err; hypothesis "", decode_s null)
//   <out-dir>/env.json  {engine, checkpoint, dtype, ep, gpu, driver, ...}
//
// This is the app's OWN shipped-model control lane: zero external cost, no
// candidate/provider calls, not subject to the bakeoff run-gate approval token
// (docs/BAKEOFF_RUN_GATE.md governs candidate/provider dispatch only; the run
// packet's own "Fresh the app controls" section calls this out as a
// pre-scoring step, not a gated dispatch). Windows-only: DirectML EP for
// Parakeet, whisper-cli.exe Vulkan CLI for Whisper.

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("openasr_controls is Windows-only (DirectML Parakeet + whisper-cli.exe Vulkan control lane)");
    std::process::exit(1);
}

#[cfg(target_os = "windows")]
fn main() -> Result<(), String> {
    windows_impl::run()
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use std::fs::File;
    use std::io::{BufWriter, Write};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Instant;

    use rightkit_dictation_engine::asr::{AsrEp, AsrRuntime};
    use rightkit_dictation_engine::canonical_polish_harness::prepare_product_input;
    use rightkit_dictation_engine::text_polish::polish_local_only;
    use rightkit_dictation_engine::whisper_win::WhisperWin;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    const UNIFIED_CHECKPOINT: &str = "unified_static15_b128_sym_bits4_timestamp_hybrid";
    const TDT_CHECKPOINT: &str = "parakeet_tdt_v3_static1500_qint8_20260722";
    const WHISPER_CHECKPOINT: &str = "whisper_turbo_q5_k.bin";

    #[derive(Debug, Clone, Deserialize)]
    struct ManifestRow {
        id: String,
        path: String,
        #[allow(dead_code)]
        duration_s: f64,
        #[allow(dead_code)]
        #[serde(default)]
        sha256: Option<String>,
    }

    #[derive(Debug, Serialize)]
    struct HypRow<'a> {
        id: &'a str,
        hypothesis: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        raw_hypothesis: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        l0_polish_ms: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        control_intent: Option<&'static str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ai_transform: Option<String>,
        decode_s: Option<f64>,
        status: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        simulated_stop_tail_ms: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        total_compute_ms: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        scheduled_window_decode_ms: Option<Vec<f64>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        scheduled_background_windows: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tail_audio_s: Option<f64>,
    }

    struct ScheduledResult {
        text: String,
        total_compute_s: f64,
        stop_tail_s: f64,
        window_decode_ms: Vec<f64>,
        background_windows: usize,
        tail_audio_s: f64,
    }

    #[derive(Clone, Copy, Debug)]
    struct RollingStaticWindow {
        decode_start: usize,
        decode_end: usize,
        stable_start: usize,
        stable_end: usize,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Engine {
        Unified,
        Tdt,
        Whisper,
    }

    impl Engine {
        fn parse(s: &str) -> Result<Self, String> {
            match s.trim().to_ascii_lowercase().as_str() {
                "unified" => Ok(Engine::Unified),
                "tdt" => Ok(Engine::Tdt),
                "whisper" => Ok(Engine::Whisper),
                other => Err(format!(
                    "unknown --engine {other:?} (expected unified|tdt|whisper)"
                )),
            }
        }

        fn slug(self) -> &'static str {
            match self {
                Engine::Unified => "unified",
                Engine::Tdt => "tdt",
                Engine::Whisper => "whisper",
            }
        }

        fn checkpoint(self) -> &'static str {
            match self {
                Engine::Unified => UNIFIED_CHECKPOINT,
                Engine::Tdt => TDT_CHECKPOINT,
                Engine::Whisper => WHISPER_CHECKPOINT,
            }
        }

        fn dtype(self) -> &'static str {
            match self {
                Engine::Unified => {
                    "encoder INT4 MatMulNBits block128 symmetric; decoder/joint INT8 \
                     (docs/UNIFIED_ASR_LOCK_2026-06-27.md)"
                }
                Engine::Tdt => {
                    "encoder/decoder INT4 MatMulNBits block64 symmetric (TDT v3 protect \
                     checkpoint naming; same quant family as Unified, block size 64 not 128)"
                }
                Engine::Whisper => "GGUF q5_k (whisper.cpp-style quant)",
            }
        }

        fn ship_policy(self) -> &'static str {
            match self {
                Engine::Unified | Engine::Tdt => {
                    "padded_window: 15s window, 2.24s trailing-silence search, timestamp \
                     commit, max 16-char overlap trim (AsrRuntime::transcribe_result -> \
                     transcribe_padded_window)"
                }
                Engine::Whisper => {
                    "whisper-cli.exe Vulkan CLI per utterance, -nt -np, lang=en \
                     (WhisperWin::transcribe_lang)"
                }
            }
        }

        fn locked_reference_wer(self) -> Option<&'static str> {
            match self {
                Engine::Unified => Some(
                    "6.265% (docs/UNIFIED_ASR_LOCK_2026-06-27.md line 28, padded_window \
                     hard/no-buffer, Pico-224 no-Chime) — screening reference only, not an \
                     an acceptance basis for this run",
                ),
                Engine::Tdt => Some(
                    "8.001% (docs/UNIFIED_ASR_LOCK_2026-06-27.md line 37, TDT SW silence-cut \
                     hard/no-buffer) — screening reference only",
                ),
                Engine::Whisper => None,
            }
        }
    }

    /// Resolve `ORT_DYLIB_PATH` before any ORT/AsrRuntime use, same purpose as
    /// `main.rs::configure_ort_dylib` (the sidecar binary calls that at boot;
    /// `cargo run --example` does not run `main.rs` at all, so an example that
    /// touches `AsrRuntime` must resolve this itself or the DirectML EP setup
    /// inside `ort`'s "load-dynamic" build hangs indefinitely with no error and
    /// no CPU/memory growth instead of failing fast — reproduced during this
    /// runner's smoke test, 2026-07-17). Respects a pre-set `ORT_DYLIB_PATH`.
    ///
    /// Candidates mirror `main.rs` (exe-sibling, packaged `resources/runtime`)
    /// plus BOTH the `.venv-build-dml` name `main.rs` hardcodes and the
    /// `.venv-dml-eval` name that actually exists in this checkout as of
    /// 2026-07-17 — `main.rs`'s dev fallback path is stale on this box; this
    /// runner does not edit `main.rs` (out of scope, shipped source), it only
    /// widens its own candidate list so `cargo run --example` works standalone.
    fn configure_ort_dylib() {
        if std::env::var("ORT_DYLIB_PATH")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
        {
            return;
        }
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let repo_root = repo_root();
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(dir) = &exe_dir {
            candidates.push(dir.join("onnxruntime.dll"));
            candidates.push(dir.join("resources/runtime/onnxruntime.dll"));
        }
        candidates.push(
            repo_root.join(".venv-build-dml/Lib/site-packages/onnxruntime/capi/onnxruntime.dll"),
        );
        candidates.push(repo_root.join(".venv-dml-eval/Scripts/onnxruntime.dll"));
        candidates.push(
            repo_root.join(".venv-dml-eval/Lib/site-packages/onnxruntime/capi/onnxruntime.dll"),
        );

        // A DirectML-capable onnxruntime.dll REQUIRES DirectML.dll beside it.
        // Without it, ORT registers the DML EP, the registration silently fails,
        // and the vendored crate falls back to CPU while still reporting
        // `ep=dml` — so the run looks GPU-accelerated and is not.
        //
        // That is exactly what `.venv-dml-eval/Scripts/onnxruntime.dll` is: a
        // 15.3 MB CPU-only build with no DirectML.dll sibling, sitting earlier in
        // this list than the real 20.1 MB DML build in the same venv's
        // `capi/`. Measured 2026-07-20 on the compact TDT package: 3.59x RTFx via
        // the CPU-only DLL vs 15.42x via the correct one, byte-identical
        // transcripts. Every eval run through this harness before that date was a
        // CPU number wearing a GPU label.
        //
        // So: prefer a candidate whose directory also contains DirectML.dll, and
        // only fall back to a bare onnxruntime.dll if no paired one exists.
        let has_directml = |p: &PathBuf| {
            p.parent()
                .map(|d| d.join("DirectML.dll").exists())
                .unwrap_or(false)
        };
        let paired = candidates.iter().find(|p| p.exists() && has_directml(p));
        let chosen = paired.cloned().or_else(|| {
            candidates
                .iter()
                .find(|p| p.exists())
                .inspect(|p| {
                    eprintln!(
                        "warning: {} has no DirectML.dll sibling — the DML EP will \
                         silently fall back to CPU while still reporting ep=dml",
                        p.display()
                    );
                })
                .cloned()
        });

        if let Some(dll) = chosen {
            println!(
                "ORT_DYLIB_PATH={} (DirectML.dll sibling: {})",
                dll.display(),
                has_directml(&dll)
            );
            if let Err(error) = rightkit_onnx::configure_runtime_with_options(
                [rightkit_onnx::RuntimeCandidate::new(
                    &dll,
                    rightkit_onnx::CandidateSource::Explicit,
                )],
                &rightkit_onnx::RuntimeBindingOptions {
                    force_replace: true,
                    ..Default::default()
                },
            ) {
                eprintln!("warning: ORT_DYLIB_PATH not bound: {error}");
            }
        } else {
            eprintln!(
                "warning: ORT_DYLIB_PATH unresolved; AsrRuntime::load will likely hang or fail"
            );
        }
    }

    pub fn run() -> Result<(), String> {
        configure_ort_dylib();
        let args: Vec<String> = std::env::args().collect();
        let repo_root = repo_root();
        let engine = Engine::parse(&flag_value(&args, "--engine").ok_or_else(|| {
            "usage: openasr_controls --engine <unified|tdt|whisper> [--manifest <path>] \
                 [--models-root <path>] [--out-dir <path>] [--limit <n>] \
                 [--scheduled-window-seconds <n>] [--rolling-static-commit-seconds <n>] \
                 [--rolling-static-quiet-max-seconds <n>] [--dynamic-full-sequence]"
                .to_string()
        })?)?;
        let manifest_path = flag_value(&args, "--manifest").map_or_else(
            || repo_root.join("tauri-app-next/scripts/open-asr-bakeoff/canonical-eval/candidate_manifest.json"),
            PathBuf::from,
        );
        let models_root = flag_value(&args, "--models-root")
            .map_or_else(|| repo_root.join("model_registry/final/asr"), PathBuf::from);
        let out_dir = PathBuf::from(flag_value(&args, "--out-dir").unwrap_or_else(|| {
            repo_root
                .join("tauri-app-next/scripts/open-asr-bakeoff/out")
                .join(format!("control-{}", engine.slug()))
                .display()
                .to_string()
        }));
        let limit = flag_value(&args, "--limit").and_then(|v| v.parse::<usize>().ok());
        let scheduled_static = args.iter().any(|arg| arg == "--scheduled-static");
        let scheduled_window_seconds = scheduled_window_seconds(&args)?;
        let rolling_static_commit_seconds = rolling_static_commit_seconds(&args)?;
        let rolling_static_quiet_max_seconds =
            rolling_static_quiet_max_seconds(&args, rolling_static_commit_seconds)?;
        let dynamic_full_sequence = args.iter().any(|arg| arg == "--dynamic-full-sequence");
        let l0_polish = args.iter().any(|arg| arg == "--l0-polish");
        validate_decode_modes(
            engine,
            scheduled_static,
            rolling_static_commit_seconds,
            dynamic_full_sequence,
        )?;

        if !manifest_path.exists() {
            return Err(format!("manifest not found: {}", manifest_path.display()));
        }
        let mut rows: Vec<ManifestRow> = serde_json::from_str(
            &std::fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("parse manifest {}: {e}", manifest_path.display()))?;
        if let Some(n) = limit {
            rows.truncate(n);
        }
        if rows.is_empty() {
            return Err("manifest has zero rows after applying --limit".to_string());
        }

        std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
        let hyp_path = out_dir.join("hypotheses.jsonl");
        let env_path = out_dir.join("env.json");

        println!(
            "openasr_controls engine={} rows={} manifest={} models_root={} out_dir={}",
            engine.slug(),
            rows.len(),
            manifest_path.display(),
            models_root.display(),
            out_dir.display()
        );

        let ep = match engine {
            Engine::Unified | Engine::Tdt => {
                requested_ep(&args)?.unwrap_or_else(AsrEp::resolve_default)
            }
            Engine::Whisper => AsrEp::Cpu, // whisper-cli.exe owns its own Vulkan device select
        };

        let mut writer = BufWriter::new(File::create(&hyp_path).map_err(|e| e.to_string())?);
        let mut ok = 0usize;
        let mut empty = 0usize;
        let mut unsupported = 0usize;
        let load_started = Instant::now();

        match engine {
            Engine::Unified | Engine::Tdt => {
                let backend = match engine {
                    Engine::Unified => "parakeet-unified",
                    Engine::Tdt => "parakeet-tdt",
                    Engine::Whisper => unreachable!(),
                };
                // configured_backend() reads RIGHTKIT_ASR_BACKEND first
                // (rightkit-dictation-inference/src/asr/models.rs), so this
                // selects the exact production model_subdir per engine without
                // touching persisted settings.
                std::env::set_var("RIGHTKIT_ASR_BACKEND", backend);
                let mut full_sequence_model = if dynamic_full_sequence {
                    match AsrRuntime::load(&models_root, ep)
                        .map_err(|e| format!("AsrRuntime::load(dynamic-full-sequence): {e}"))?
                    {
                        runtime @ AsrRuntime::Parakeet(..) => Some(runtime),
                        _ => {
                            return Err(
                                "dynamic full-sequence loader did not select Parakeet".into()
                            )
                        }
                    }
                } else {
                    None
                };
                let mut model = if dynamic_full_sequence {
                    None
                } else {
                    Some(
                        AsrRuntime::load(&models_root, ep)
                            .map_err(|e| format!("AsrRuntime::load({}): {e}", engine.slug()))?,
                    )
                };
                let load_s = load_started.elapsed().as_secs_f64();
                println!("model loaded in {load_s:.2}s ep={}", ep.as_str());

                // One warm-up decode, excluded from every row's decode_s.
                if let Some(first) = rows.first() {
                    let (samples, _) = load_wav(Path::new(&first.path))?;
                    if let Some(model) = full_sequence_model.as_mut() {
                        let _ = model.transcribe_result(&samples);
                    } else if let Some(model) = model.as_mut() {
                        let _ = model.transcribe_result(&samples);
                    }
                }

                for row in &rows {
                    let (samples, _duration_s) = load_wav(Path::new(&row.path))?;
                    let (text_result, decode_s, scheduled) = if let Some(model) =
                        full_sequence_model.as_mut()
                    {
                        let t0 = Instant::now();
                        let result = model.transcribe_result(&samples).map(|result| result.text);
                        (
                            result.map_err(|e| e.to_string()),
                            t0.elapsed().as_secs_f64(),
                            None,
                        )
                    } else if let Some(commit_seconds) = rolling_static_commit_seconds {
                        match transcribe_rolling_static(
                            model.as_mut().expect("runtime model must exist"),
                            &samples,
                            commit_seconds,
                            rolling_static_quiet_max_seconds,
                        ) {
                            Ok(result) => {
                                let decode_s = result.total_compute_s;
                                (Ok(result.text.clone()), decode_s, Some(result))
                            }
                            Err(error) => (Err(error), 0.0, None),
                        }
                    } else if scheduled_static {
                        match transcribe_scheduled_static(
                            model.as_mut().expect("runtime model must exist"),
                            &samples,
                            scheduled_window_seconds,
                        ) {
                            Ok(result) => {
                                let decode_s = result.total_compute_s;
                                (Ok(result.text.clone()), decode_s, Some(result))
                            }
                            Err(error) => (Err(error), 0.0, None),
                        }
                    } else {
                        let t0 = Instant::now();
                        let result = model
                            .as_mut()
                            .expect("runtime model must exist")
                            .transcribe_result(&samples)
                            .map(|result| result.text);
                        (result, t0.elapsed().as_secs_f64(), None)
                    };
                    let tag = record_row(
                        &mut writer,
                        row,
                        text_result,
                        decode_s,
                        scheduled,
                        l0_polish,
                    )?;
                    match tag {
                        "ok" => ok += 1,
                        "empty" => empty += 1,
                        _ => unsupported += 1,
                    }
                    println!(
                        "{} {} {:.3}s status={}",
                        engine.slug(),
                        row.id,
                        decode_s,
                        tag
                    );
                }
            }
            Engine::Whisper => {
                std::env::set_var(
                    "RIGHTKIT_WHISPER_GGUF",
                    models_root.join(WHISPER_CHECKPOINT),
                );
                let whisper_engine =
                    WhisperWin::load(&models_root).map_err(|e| format!("WhisperWin::load: {e}"))?;
                let load_s = load_started.elapsed().as_secs_f64();
                println!("whisper engine loaded in {load_s:.2}s");

                if let Some(first) = rows.first() {
                    let (samples, _) = load_wav(Path::new(&first.path))?;
                    let _ = whisper_engine.transcribe_lang(&samples, "en");
                }

                for row in &rows {
                    let (samples, _duration_s) = load_wav(Path::new(&row.path))?;
                    let t0 = Instant::now();
                    let text_result = whisper_engine.transcribe_lang(&samples, "en");
                    let decode_s = t0.elapsed().as_secs_f64();
                    let tag = record_row(&mut writer, row, text_result, decode_s, None, l0_polish)?;
                    match tag {
                        "ok" => ok += 1,
                        "empty" => empty += 1,
                        _ => unsupported += 1,
                    }
                    println!(
                        "{} {} {:.3}s status={}",
                        engine.slug(),
                        row.id,
                        decode_s,
                        tag
                    );
                }
            }
        }

        writer.flush().map_err(|e| e.to_string())?;
        println!(
            "wrote {} rows={} ok={} empty={} unsupported={}",
            hyp_path.display(),
            rows.len(),
            ok,
            empty,
            unsupported
        );

        let (gpu, driver) = detect_gpu();
        let model_dir_display = if engine == Engine::Whisper {
            models_root.join(engine.checkpoint()).display().to_string()
        } else {
            models_root.join(engine.checkpoint()).display().to_string()
        };
        let env_json = json!({
            "engine": engine.slug(),
            "checkpoint": engine.checkpoint(),
            "model_dir": model_dir_display,
            "dtype": engine.dtype(),
            "ep": ep.as_str(),
            "ship_policy": engine.ship_policy(),
            "scheduled_static": scheduled_static,
            "scheduled_window_seconds": scheduled_window_seconds,
            "rolling_static_commit_seconds": rolling_static_commit_seconds,
            "rolling_static_quiet_max_seconds": rolling_static_quiet_max_seconds,
            "dynamic_full_sequence": dynamic_full_sequence,
            "l0_polish": l0_polish,
            "locked_reference_wer": engine.locked_reference_wer(),
            "reconditioned": false,
            "reconditioned_note": "input WAVs are already condition_for_asr-conditioned by condition-corpus.mjs (policy asr_simple_gain_hpf); AsrRuntime::transcribe_result / transcribe_padded_window and WhisperWin::transcribe_lang do not call condition_for_asr internally (verified against heardright-inference/src/asr/runtime.rs and heardright-inference/src/whisper_win.rs, 2026-07-17) — see this file's header comment for the full grep trail.",
            "manifest": manifest_path.display().to_string(),
            "rows_total": rows.len(),
            "rows_ok": ok,
            "rows_empty": empty,
            "rows_unsupported": unsupported,
            "runtime": "rightkit_dictation_engine (this repo's shipped sidecar crate), example openasr_controls.rs",
            "gpu": gpu,
            "driver": driver,
            "os": "windows",
            "generated_at": chrono::Utc::now().to_rfc3339(),
        });
        std::fs::write(
            &env_path,
            serde_json::to_string_pretty(&env_json).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        println!("wrote {}", env_path.display());
        Ok(())
    }

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// Classify one decode outcome, write its `hypotheses.jsonl` row, flush
    /// immediately (checkpoint-per-clip so a killed process leaves usable
    /// partial progress), and return the status tag for the caller's tally.
    fn record_row(
        writer: &mut BufWriter<File>,
        row: &ManifestRow,
        text_result: Result<String, String>,
        decode_s: f64,
        scheduled: Option<ScheduledResult>,
        l0_polish: bool,
    ) -> Result<&'static str, String> {
        let (
            hypothesis,
            raw_hypothesis,
            l0_polish_ms,
            control_intent,
            ai_transform,
            decode_s_out,
            status,
        ) = match text_result {
            Ok(text) => {
                let trimmed = text.trim().to_string();
                if trimmed.is_empty() {
                    (
                        String::new(),
                        l0_polish.then(String::new),
                        l0_polish.then_some(0.0),
                        None,
                        None,
                        Some(decode_s),
                        "empty",
                    )
                } else if l0_polish {
                    let started = Instant::now();
                    let prepared = prepare_product_input(&trimmed);
                    let polished = if prepared.cancelled {
                        String::new()
                    } else {
                        polish_local_only(&prepared.text)
                    };
                    (
                        polished,
                        Some(trimmed),
                        Some(started.elapsed().as_secs_f64() * 1_000.0),
                        prepared.control_intent,
                        prepared.ai_transform,
                        Some(decode_s),
                        "ok",
                    )
                } else {
                    (trimmed, None, None, None, None, Some(decode_s), "ok")
                }
            }
            Err(e) => {
                eprintln!("row {} decode error: {e}", row.id);
                (String::new(), None, None, None, None, None, "unsupported")
            }
        };
        let hyp_row = HypRow {
            id: &row.id,
            hypothesis,
            raw_hypothesis,
            l0_polish_ms,
            control_intent,
            ai_transform,
            decode_s: decode_s_out,
            status,
            simulated_stop_tail_ms: scheduled
                .as_ref()
                .map(|result| result.stop_tail_s * 1_000.0),
            total_compute_ms: scheduled
                .as_ref()
                .map(|result| result.total_compute_s * 1_000.0),
            scheduled_window_decode_ms: scheduled
                .as_ref()
                .map(|result| result.window_decode_ms.clone()),
            scheduled_background_windows: scheduled
                .as_ref()
                .map(|result| result.background_windows),
            tail_audio_s: scheduled.as_ref().map(|result| result.tail_audio_s),
        };
        let line = serde_json::to_string(&hyp_row).map_err(|e| e.to_string())?;
        writeln!(writer, "{line}").map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
        Ok(status)
    }

    fn transcribe_scheduled_static(
        model: &mut AsrRuntime,
        samples: &[f32],
        window_seconds: usize,
    ) -> Result<ScheduledResult, String> {
        const SAMPLE_RATE: usize = 16_000;
        const PADDING: usize = 2_240 * SAMPLE_RATE / 1_000;
        let window = window_seconds * SAMPLE_RATE;
        let duration_s = samples.len() as f64 / SAMPLE_RATE as f64;
        let mut text = String::new();
        let mut start = 0usize;
        let mut schedule_finish_s = 0.0f64;
        let mut total_compute_s = 0.0f64;
        let mut window_decode_ms = Vec::new();
        let mut background_windows = 0usize;

        while start + window < samples.len() {
            let window_end = start + window;
            let target_end = start + window - PADDING;
            let cut = quiet_cut_in(&samples, target_end, window_end).unwrap_or(window_end);
            let t0 = Instant::now();
            let result = model.transcribe_result(&samples[start..cut])?;
            let elapsed_s = t0.elapsed().as_secs_f64();
            append_with_overlap(&mut text, &result.text);
            schedule_finish_s =
                schedule_finish_s.max(window_end as f64 / SAMPLE_RATE as f64) + elapsed_s;
            total_compute_s += elapsed_s;
            window_decode_ms.push(elapsed_s * 1_000.0);
            background_windows += 1;
            start = cut.max(start + 1);
        }

        let tail_audio_s = (samples.len() - start) as f64 / SAMPLE_RATE as f64;
        let t0 = Instant::now();
        let result = model.transcribe_result(&samples[start..])?;
        let elapsed_s = t0.elapsed().as_secs_f64();
        append_with_overlap(&mut text, &result.text);
        schedule_finish_s = schedule_finish_s.max(duration_s) + elapsed_s;
        total_compute_s += elapsed_s;
        window_decode_ms.push(elapsed_s * 1_000.0);

        Ok(ScheduledResult {
            text: text.trim().to_string(),
            total_compute_s,
            stop_tail_s: (schedule_finish_s - duration_s).max(0.0),
            window_decode_ms,
            background_windows,
            tail_audio_s,
        })
    }

    fn rolling_static_windows(
        samples: &[f32],
        commit_seconds: usize,
        quiet_max_seconds: Option<usize>,
    ) -> Vec<RollingStaticWindow> {
        let sample_rate = 16_000usize;
        let context = 15 * sample_rate;
        let mut windows = Vec::new();
        let mut stable_start = 0usize;
        while stable_start < samples.len() {
            let stable_end = quiet_max_seconds
                .map(|max_seconds| {
                    next_quiet_commit(samples, stable_start, commit_seconds, max_seconds)
                })
                .unwrap_or_else(|| {
                    (stable_start + commit_seconds * sample_rate).min(samples.len())
                });
            let stable_len = stable_end - stable_start;
            let remaining_context = context.saturating_sub(stable_len);
            let left = remaining_context / 2;
            let right = remaining_context - left;
            windows.push(RollingStaticWindow {
                decode_start: stable_start.saturating_sub(left),
                decode_end: (stable_end + right).min(samples.len()),
                stable_start,
                stable_end,
            });
            stable_start = stable_end;
        }
        windows
    }

    fn transcribe_rolling_static(
        model: &mut AsrRuntime,
        samples: &[f32],
        commit_seconds: usize,
        quiet_max_seconds: Option<usize>,
    ) -> Result<ScheduledResult, String> {
        let duration_s = samples.len() as f64 / 16_000.0;
        let mut text = String::new();
        let mut committed_samples = 0usize;
        let mut total_compute_s = 0.0f64;
        let mut schedule_finish_s = 0.0f64;
        let mut window_decode_ms = Vec::new();
        let mut background_windows = 0usize;

        for window in rolling_static_windows(samples, commit_seconds, quiet_max_seconds) {
            if window.decode_end >= samples.len() {
                break;
            }
            let started = Instant::now();
            let result =
                model.transcribe_result(&samples[window.decode_start..window.decode_end])?;
            let decode_s = started.elapsed().as_secs_f64();
            total_compute_s += decode_s;
            window_decode_ms.push(decode_s * 1_000.0);
            schedule_finish_s =
                schedule_finish_s.max(window.decode_end as f64 / 16_000.0) + decode_s;
            let offset_s = window.decode_start as f32 / 16_000.0;
            let stable_start_s = window.stable_start as f32 / 16_000.0;
            let stable_end_s = window.stable_end as f32 / 16_000.0;
            let mut piece = String::new();
            for token in result.tokens {
                let token_start = offset_s + token.start;
                if token_start >= stable_start_s && token_start < stable_end_s {
                    piece.push_str(&token.text);
                }
            }
            append_with_overlap(&mut text, &piece);
            committed_samples = window.stable_end;
            background_windows += 1;
        }

        let tail_started = Instant::now();
        let tail = model.transcribe_result(&samples[committed_samples..])?;
        let stop_tail_s = tail_started.elapsed().as_secs_f64();
        total_compute_s += stop_tail_s;
        append_with_overlap(&mut text, &tail.text);
        schedule_finish_s = schedule_finish_s.max(duration_s) + stop_tail_s;

        Ok(ScheduledResult {
            text: text.trim().to_string(),
            total_compute_s,
            stop_tail_s: (schedule_finish_s - duration_s).max(0.0),
            window_decode_ms,
            background_windows,
            tail_audio_s: (samples.len() - committed_samples) as f64 / 16_000.0,
        })
    }

    fn quiet_cut_in(samples: &[f32], lo: usize, hi: usize) -> Option<usize> {
        const SILENCE_WINDOW: usize = 200 * 16_000 / 1_000;
        const SILENCE_HOP: usize = 100 * 16_000 / 1_000;
        if hi <= lo || hi.saturating_sub(lo) < SILENCE_WINDOW {
            return None;
        }
        let mut best: Option<(usize, f32)> = None;
        let mut offset = lo;
        while offset + SILENCE_WINDOW <= hi {
            let sum_sq: f32 = samples[offset..offset + SILENCE_WINDOW]
                .iter()
                .map(|sample| sample * sample)
                .sum();
            let level = (sum_sq / SILENCE_WINDOW as f32).sqrt();
            if best.map(|(_, prior)| level < prior).unwrap_or(true) {
                best = Some((offset + SILENCE_WINDOW / 2, level));
            }
            offset += SILENCE_HOP;
        }
        best.map(|(cut, _)| cut)
    }

    fn next_quiet_commit(
        samples: &[f32],
        start: usize,
        min_seconds: usize,
        max_seconds: usize,
    ) -> usize {
        const SAMPLE_RATE: usize = 16_000;
        let lo = (start + min_seconds * SAMPLE_RATE).min(samples.len());
        let hi = (start + max_seconds * SAMPLE_RATE).min(samples.len());
        quiet_cut_in(samples, lo, hi).unwrap_or(hi)
    }

    fn append_with_overlap(buffer: &mut String, next: &str) {
        const MAX_OVERLAP: usize = 16;
        if next.is_empty() {
            return;
        }
        if buffer.is_empty() {
            buffer.push_str(next);
            return;
        }
        let max = buffer.len().min(next.len()).min(MAX_OVERLAP);
        let overlap = (1..=max)
            .filter(|len| {
                buffer.is_char_boundary(buffer.len() - len) && next.is_char_boundary(*len)
            })
            .filter(|len| buffer[buffer.len() - len..].eq_ignore_ascii_case(&next[..*len]))
            .max()
            .unwrap_or(0);
        let remainder = &next[overlap..];
        if remainder.is_empty() {
            return;
        }
        if overlap == 0
            && !buffer.ends_with(char::is_whitespace)
            && !remainder.starts_with(char::is_whitespace)
        {
            buffer.push(' ');
        }
        buffer.push_str(remainder);
    }

    fn load_wav(path: &Path) -> Result<(Vec<f32>, f64), String> {
        let wav = rightkit_audio::wav::read_wav_mono_f32(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let duration_s = wav.samples.len() as f64 / wav.sample_rate.max(1) as f64;
        Ok((wav.samples, duration_s))
    }

    /// Best-effort GPU/driver identification for env.json. Never fatal — a
    /// failed probe reports "unknown", it does not abort the run.
    fn detect_gpu() -> (String, String) {
        if let Some((gpu, driver)) = detect_gpu_wmic() {
            return (gpu, driver);
        }
        if let Some((gpu, driver)) = detect_gpu_powershell() {
            return (gpu, driver);
        }
        ("unknown".to_string(), "unknown".to_string())
    }

    fn detect_gpu_wmic() -> Option<(String, String)> {
        let out = Command::new("wmic")
            .args([
                "path",
                "win32_VideoController",
                "get",
                "Name,DriverVersion",
                "/format:list",
            ])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        parse_key_value_gpu(&String::from_utf8_lossy(&out.stdout))
    }

    fn detect_gpu_powershell() -> Option<(String, String)> {
        let out = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Get-CimInstance Win32_VideoController | Select-Object -First 1 Name,DriverVersion | Format-List",
            ])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        parse_key_value_gpu(&String::from_utf8_lossy(&out.stdout))
    }

    fn parse_key_value_gpu(text: &str) -> Option<(String, String)> {
        let mut name = None;
        let mut driver = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(v) = line
                .strip_prefix("Name=")
                .or_else(|| line.strip_prefix("Name :"))
            {
                let v = v.trim();
                if !v.is_empty() && name.is_none() {
                    name = Some(v.to_string());
                }
            }
            if let Some(v) = line
                .strip_prefix("DriverVersion=")
                .or_else(|| line.strip_prefix("DriverVersion :"))
            {
                let v = v.trim();
                if !v.is_empty() && driver.is_none() {
                    driver = Some(v.to_string());
                }
            }
        }
        match (name, driver) {
            (Some(n), Some(d)) => Some((n, d)),
            (Some(n), None) => Some((n, "unknown".to_string())),
            _ => None,
        }
    }

    fn flag_value(args: &[String], name: &str) -> Option<String> {
        args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone())
    }

    fn scheduled_window_seconds(args: &[String]) -> Result<usize, String> {
        let seconds = flag_value(args, "--scheduled-window-seconds")
            .unwrap_or_else(|| "15".to_string())
            .parse::<usize>()
            .map_err(|_| "--scheduled-window-seconds must be an integer".to_string())?;
        if seconds < 3 {
            return Err("--scheduled-window-seconds must be at least 3".to_string());
        }
        Ok(seconds)
    }

    fn requested_ep(args: &[String]) -> Result<Option<AsrEp>, String> {
        match flag_value(args, "--ep").as_deref() {
            None | Some("auto") => Ok(None),
            Some("cpu") => Ok(Some(AsrEp::Cpu)),
            Some("dml") => Ok(Some(AsrEp::Dml)),
            #[cfg(all(target_os = "windows", feature = "cuda-bench"))]
            Some("cuda") => Ok(Some(AsrEp::Cuda)),
            Some(value) => Err(format!(
                "unsupported --ep {value}; expected auto, cpu, dml, or cuda"
            )),
        }
    }

    fn rolling_static_commit_seconds(args: &[String]) -> Result<Option<usize>, String> {
        let Some(value) = flag_value(args, "--rolling-static-commit-seconds") else {
            return Ok(None);
        };
        let seconds = value
            .parse::<usize>()
            .map_err(|_| "--rolling-static-commit-seconds must be an integer".to_string())?;
        if !(1..15).contains(&seconds) {
            return Err("--rolling-static-commit-seconds must be between 1 and 14".to_string());
        }
        Ok(Some(seconds))
    }

    fn rolling_static_quiet_max_seconds(
        args: &[String],
        commit_seconds: Option<usize>,
    ) -> Result<Option<usize>, String> {
        let Some(value) = flag_value(args, "--rolling-static-quiet-max-seconds") else {
            return Ok(None);
        };
        let seconds = value
            .parse::<usize>()
            .map_err(|_| "--rolling-static-quiet-max-seconds must be an integer".to_string())?;
        let Some(min_seconds) = commit_seconds else {
            return Err(
                "--rolling-static-quiet-max-seconds requires --rolling-static-commit-seconds"
                    .to_string(),
            );
        };
        if seconds <= min_seconds || seconds > 15 {
            return Err(
                "--rolling-static-quiet-max-seconds must exceed the commit minimum and be at most 15"
                    .to_string(),
            );
        }
        Ok(Some(seconds))
    }

    fn validate_decode_modes(
        engine: Engine,
        scheduled_static: bool,
        rolling_static_commit_seconds: Option<usize>,
        dynamic_full_sequence: bool,
    ) -> Result<(), String> {
        if scheduled_static && engine != Engine::Tdt {
            return Err("--scheduled-static is supported only with --engine tdt".to_string());
        }
        if rolling_static_commit_seconds.is_some() && engine != Engine::Tdt {
            return Err(
                "--rolling-static-commit-seconds is supported only with --engine tdt".to_string(),
            );
        }
        if dynamic_full_sequence && engine != Engine::Tdt {
            return Err("--dynamic-full-sequence is supported only with --engine tdt".to_string());
        }
        let selected = usize::from(scheduled_static)
            + usize::from(rolling_static_commit_seconds.is_some())
            + usize::from(dynamic_full_sequence);
        if selected > 1 {
            return Err(
                "choose exactly one of --scheduled-static, --rolling-static-commit-seconds, or --dynamic-full-sequence"
                    .to_string(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn explicit_cpu_provider_is_honored() {
            let args = vec![
                "openasr_controls".to_string(),
                "--ep".to_string(),
                "cpu".to_string(),
            ];

            assert_eq!(requested_ep(&args).unwrap(), Some(AsrEp::Cpu));
        }

        #[cfg(all(target_os = "windows", feature = "cuda-bench"))]
        #[test]
        fn explicit_cuda_provider_is_honored() {
            let args = vec![
                "openasr_controls".to_string(),
                "--ep".to_string(),
                "cuda".to_string(),
            ];

            assert_eq!(requested_ep(&args).unwrap(), Some(AsrEp::Cuda));
        }

        #[test]
        fn provider_defaults_to_auto() {
            let args = vec!["openasr_controls".to_string()];

            assert_eq!(requested_ep(&args).unwrap(), None);
        }

        #[test]
        fn scheduled_window_accepts_five_seconds() {
            let args = vec![
                "openasr_controls".to_string(),
                "--scheduled-window-seconds".to_string(),
                "5".to_string(),
            ];

            assert_eq!(scheduled_window_seconds(&args).unwrap(), 5);
        }

        #[test]
        fn scheduled_window_defaults_to_fifteen_seconds() {
            let args = vec!["openasr_controls".to_string()];

            assert_eq!(scheduled_window_seconds(&args).unwrap(), 15);
        }

        #[test]
        fn scheduled_window_rejects_zero_seconds() {
            let args = vec![
                "openasr_controls".to_string(),
                "--scheduled-window-seconds".to_string(),
                "0".to_string(),
            ];

            assert_eq!(
                scheduled_window_seconds(&args).unwrap_err(),
                "--scheduled-window-seconds must be at least 3"
            );
        }

        #[test]
        fn scheduled_window_must_exceed_quiet_search() {
            let args = vec![
                "openasr_controls".to_string(),
                "--scheduled-window-seconds".to_string(),
                "2".to_string(),
            ];

            assert_eq!(
                scheduled_window_seconds(&args).unwrap_err(),
                "--scheduled-window-seconds must be at least 3"
            );
        }

        #[test]
        fn rolling_static_windows_cover_audio_once_with_fifteen_second_context() {
            for commit_seconds in [2, 3] {
                let total = 20 * 16_000;
                let samples = vec![0.5; total];
                let windows = rolling_static_windows(&samples, commit_seconds, None);
                assert_eq!(windows.first().unwrap().stable_start, 0);
                assert_eq!(windows.last().unwrap().stable_end, total);
                for pair in windows.windows(2) {
                    assert_eq!(pair[0].stable_end, pair[1].stable_start);
                }
                assert!(windows
                    .iter()
                    .all(|window| window.decode_end - window.decode_start <= 15 * 16_000));
            }
        }

        #[test]
        fn rolling_static_quiet_commit_uses_pause_between_three_and_five_seconds() {
            let mut samples = vec![0.5; 8 * 16_000];
            samples[4 * 16_000..4 * 16_000 + 3_200].fill(0.0);

            let cut = next_quiet_commit(&samples, 0, 3, 5);

            assert_eq!(cut, 4 * 16_000 + 1_600);
        }

        #[test]
        fn rolling_static_quiet_search_accepts_fifteen_second_endpoint() {
            let args = vec![
                "openasr_controls".to_string(),
                "--rolling-static-commit-seconds".to_string(),
                "7".to_string(),
                "--rolling-static-quiet-max-seconds".to_string(),
                "15".to_string(),
            ];

            assert_eq!(
                rolling_static_quiet_max_seconds(&args, Some(7)).unwrap(),
                Some(15)
            );
        }

        #[test]
        fn dynamic_full_sequence_cannot_be_combined_with_scheduled_static() {
            assert_eq!(
                validate_decode_modes(Engine::Tdt, true, None, true).unwrap_err(),
                "choose exactly one of --scheduled-static, --rolling-static-commit-seconds, or --dynamic-full-sequence"
            );
        }
    }
}
