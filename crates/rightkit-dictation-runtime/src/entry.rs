// `rightkit-dictation-engine` — resident dictation engine sidecar.
//
// Talks to the Tauri shell over JSON-RPC over stdio. Owns the model warm
// residency, mic capture, the recording state machine, focus-target tracking,
// delivery, and the recent transcript list. The shell is a thin client.
//
// The IPC contract is the shared `EngineFrame` schema in
// `rightkit_dictation_core::engine`. This binary emits and consumes `EngineFrame` JSON
// over stdin/stdout, one frame per line, so the channel is trivially
// recoverable.

use std::sync::Arc;

use parking_lot::Mutex;
use rightkit_dictation_inference::{asr, calibration};

use crate::{crash_capture, ipc, runtime::EngineRuntime};

pub fn run() -> anyhow::Result<()> {
    // First, before any thread or ORT use: an inherited ORT_DYLIB_PATH must not steer the
    // engine.
    rightkit_onnx::clear_inherited_runtime_for_release();
    init_tracing();
    // Sidecar postmortem: the shell can only dump its own process when it
    // notices the engine died. Install our own panic hook so a real dump of
    // THIS process lands in the shared crash dir before abort.
    crash_capture::install_panic_hook();
    // Every fixed root (app data, models base, KWS, ffmpeg, CoreML caches) is derived here from
    // the engine's own location and the shared path rules; nothing is passed in by the launcher.
    crate::engine_paths::install();
    configure_ort_dylib().map_err(anyhow::Error::msg)?;
    #[cfg(target_os = "windows")]
    rightkit_dictation_inference::init_windows_ort()?;

    let mut args = std::env::args();
    let _bin = args.next();
    let first = args.next();
    // One-shot roles get no launch frame: they calibrate Parakeet only.
    if matches!(first.as_deref(), Some("calibrate" | "calibrate-route")) {
        rightkit_dictation_inference::engine_paths::pin_parakeet_model_choice(
            resolve_parakeet_dir(),
        );
    }

    // Onboarding's one-shot setup process. The shell retains exactly one
    // 16 kHz mono f32LE recording, invokes this once, then receives a single
    // JSON result after every candidate has consumed those exact bytes.
    if first.as_deref() == Some("calibrate") {
        let pcm = args.next().unwrap_or_default();
        // Explicit option from the shell: `calibrate <pcm> --collect-diagnostics`.
        let collect_diagnostics = args.any(|arg| arg == "--collect-diagnostics");
        let path = std::path::PathBuf::from(&pcm);
        let bytes = std::fs::read(&path)
            .map_err(|error| anyhow::anyhow!("read calibration PCM {}: {error}", path.display()))?;
        if bytes.len() % 4 != 0 {
            return Err(anyhow::anyhow!("calibration PCM must be f32LE mono"));
        }
        let samples = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect::<Vec<_>>();
        #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
        let mut result = calibration::run_with_runner(&samples, collect_diagnostics, |route| {
            run_calibration_child(&pcm, route)
        });
        #[cfg(target_os = "macos")]
        if collect_diagnostics
            && result
                .attempts
                .first()
                .is_some_and(|attempt| attempt.status != "passed")
        {
            let policy = "default";
            let conditioned = rightkit_dictation_core::audio_conditioning::condition_for_asr(
                &samples, 16_000, &policy,
            );
            let probe_start = result
                .attempts
                .first()
                .and_then(|attempt| attempt.diagnostics.as_ref()?.get("windows")?.as_array())
                .and_then(|windows| {
                    windows
                        .iter()
                        .find(|window| {
                            window
                                .pointer("/stats/no_emission_gaps_over_2s")
                                .and_then(serde_json::Value::as_array)
                                .is_some_and(|gaps| !gaps.is_empty())
                        })
                        .or_else(|| windows.first())
                })
                .and_then(|window| window.get("window_start_offset_samples")?.as_u64())
                .and_then(|offset| usize::try_from(offset).ok())
                .unwrap_or(0)
                .min(conditioned.len());
            let models = asr::models_base(None);
            let model_dir = rightkit_dictation_inference::engine_paths::launch_model_choice()
                .coreml_model_dir
                .unwrap_or_else(|| models.join("parakeet-tdt-ultra"));
            result.compute_probe = Some(
                match rightkit_dictation_inference::coreml_asr::compute_differential_probe(
                    &model_dir,
                    &conditioned[probe_start..],
                ) {
                    Ok(mut metrics) => {
                        metrics.insert(
                            "probe_window_start_offset_samples".into(),
                            probe_start as f64,
                        );
                        metrics
                    }
                    Err(error) => {
                        tracing::error!(error, "onboarding CoreML differential probe failed");
                        std::collections::BTreeMap::from([("probe_unavailable".into(), 1.0)])
                    }
                },
            );
        }
        println!("{}", serde_json::to_string(&result)?);
        return Ok(());
    }
    if first.as_deref() == Some("calibrate-route") {
        let pcm = args.next().unwrap_or_default();
        let route = serde_json::from_str(&args.next().unwrap_or_default())?;
        let bytes = std::fs::read(&pcm)?;
        if bytes.len() % 4 != 0 {
            return Err(anyhow::anyhow!("calibration PCM must be f32LE mono"));
        }
        let samples = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string(&calibration::run_route(
                &asr::models_base(None),
                &samples,
                &route
            ))?
        );
        return Ok(());
    }

    // One-shot ANE-compile warmer: `rightkit-dictation-engine prepare-whisper`.
    // Loads the Whisper CoreML model once (and runs one throwaway inference) so
    // the OS caches its ANE compilation — the ~3-6 min first-load cost. The shell
    // runs this during install so the compile happens behind a visible
    // "Preparing model…" step instead of lazily blocking the engine at boot.
    // The engine derives the model directory itself.
    #[cfg(target_os = "macos")]
    if first.as_deref() == Some("prepare-whisper") {
        let dir = prepare_whisper_dir();
        tracing::info!("prepare-whisper: compiling ANE cache for {}", dir.display());
        match rightkit_dictation_inference::whisper_coreml::WhisperCoreMl::load(&dir) {
            Ok(w) => {
                // Exercise encoder + decoder so the full graph is ANE-compiled.
                let _ = w.transcribe_lang_windowed(&vec![0f32; 16_000], w.lang_token("auto"));
                tracing::info!("prepare-whisper: done");
                return Ok(());
            }
            Err(e) => {
                tracing::error!("prepare-whisper failed: {e}");
                std::process::exit(1);
            }
        }
    }

    // Resident dictation path (not a `calibrate`/`calibrate-route` child). The calibration
    // route (DirectML adapter) lives in process memory, so there is nothing inherited from the
    // launcher to strip.

    // `--speech-service`: headless host for other Orthic Labs apps (no stdio
    // loop, launched detached with stdin closed, so no launch frame). It always serves
    // Parakeet TDT from the derived models root.
    let headless = first.as_deref() == Some("--speech-service");
    if headless {
        rightkit_dictation_inference::engine_paths::pin_parakeet_model_choice(
            resolve_speech_service_parakeet_dir(),
        );
    } else {
        // The app's own engine: the shell's launch frame is the first stdin line, before hello
        // and before any model lease or load.
        install_launch_frame()?;
    }
    let role = crate::speech_service::role_for(headless);

    let models_base = asr::models_base(None);
    tracing::info!("models base: {}", models_base.display());

    // OS lease follows the process, so crash/kill releases it automatically.
    // During a guarded migration the parent store already owns this lease.
    let launch = rightkit_dictation_inference::engine_paths::launch_model_choice();
    let model_lease = if launch.backend.as_deref() == Some("parakeet-tdt") && !launch.load_check {
        Some(crate::speech_service::acquire_lease_with_takeover(role)?)
    } else {
        None
    };

    crate::speech_service::lease::install(role, model_lease);
    let runtime = Arc::new(Mutex::new(EngineRuntime::new(models_base)));
    if headless {
        return crate::speech_service::run_headless(runtime);
    }
    ipc::run(runtime)
}

fn run_calibration_child(
    pcm: &str,
    route: &calibration::CalibrationRoute,
) -> calibration::CalibrationAttempt {
    let failed = |status: String| calibration::CalibrationAttempt {
        route: route.clone(),
        transcript: String::new(),
        wer: None,
        latency_ms: 1,
        warm_latency_ms: None,
        deletion_run_max: 0,
        premature_ending: false,
        concentrated_deletion: false,
        status: status.clone(),
        error: Some(status),
        model_fingerprint: None,
        model_dir_sha256: None,
        compile_cache: None,
        diagnostics: None,
        words_per_second: None,
        max_silent_emission_gap_seconds: None,
    };
    let output = match std::env::current_exe().and_then(|exe| {
        let mut owned = rightkit_child_process::OwnedCommand::new(exe);
        owned
            .command_mut()
            .arg("calibrate-route")
            .arg(pcm)
            .arg(serde_json::to_string(route).unwrap_or_default())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        // The owner ties the route child to this process: it never outlives it.
        owned.kill_on_parent_exit(true);
        let mut child = owned.spawn()?;
        let stdout = child
            .take_stdout()
            .ok_or_else(|| std::io::Error::other("route process stdout pipe unavailable"))?;
        // Drain on its own thread so a large result cannot fill the pipe while we wait.
        let mut capture = rightkit_child_process::OutputCapture::capture(stdout, 64 * 1024 * 1024);
        let status = child.wait()?;
        Ok(std::process::Output {
            status,
            stdout: capture.finish().bytes,
            stderr: Vec::new(),
        })
    }) {
        Ok(output) => output,
        Err(_) => return failed("route_process_spawn_failed".into()),
    };
    if !output.status.success() {
        #[cfg(unix)]
        if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&output.status) {
            return failed(format!("route_process_signal_{signal}"));
        }
        return failed(
            output
                .status
                .code()
                .map(|code| format!("route_process_exit_{code}"))
                .unwrap_or_else(|| "route_process_exit_unknown".into()),
        );
    }
    match serde_json::from_slice::<calibration::CalibrationAttempt>(&output.stdout) {
        Ok(attempt)
            if attempt.route == *route
                && matches!(
                    attempt.status.as_str(),
                    "passed" | "quality_failed" | "recognition_provider_failed"
                ) =>
        {
            attempt
        }
        Ok(_) => failed("route_process_route_mismatch".into()),
        Err(_) => failed("route_process_invalid_output".into()),
    }
}

/// The Whisper CoreML directory `prepare-whisper` warms: the downloaded copy under the models
/// base.
#[cfg(target_os = "macos")]
fn prepare_whisper_dir() -> std::path::PathBuf {
    asr::models_base(None).join("whisper-multi")
}

/// The Parakeet directory for a role that gets no launch frame (calibration, the speech service).
///
/// It is the shell's own resolver, the one that fills the resident engine's launch frame
/// (rightkit-dictation-model-store `model_paths::resolve_model_dir`): this process's verified activation
/// (a one-shot has none), else the persisted activation record of a Ready shared-store Ultra copy,
/// else a complete app-data revision. One rule for both processes means calibration can never
/// pass on a directory the resident engine would refuse. Without it, calibration and the speech
/// service looked only under the app's own models root, which holds no Parakeet once Ultra lives in
/// the shared store, and onboarding failed with "missing model artifact".
fn resolve_parakeet_dir() -> Option<std::path::PathBuf> {
    use rightkit_dictation_models::model_paths::{resolve_model_dir, subdir_for_backend};
    let dir = resolve_model_dir(subdir_for_backend("parakeet-tdt"));
    tracing::info!(dir = ?dir, "one-shot role Parakeet directory");
    dir
}

/// The Parakeet directory for the headless `--speech-service` host. A sibling app launches this
/// engine on a machine whose the app shell may never have run (no activation record), so a
/// Ready shared-store Ultra copy is served directly, as the d8304432 fix did; the shell's own
/// resolver is the fallback. The calibration one-shots keep the shell's resolver alone, so
/// calibration never passes on a directory the resident engine would refuse.
fn resolve_speech_service_parakeet_dir() -> Option<std::path::PathBuf> {
    use rightkit_dictation_models::shared::store;
    let dir = store::resolve_ready_dir(
        &store::paths::models_root(),
        &rightkit_dictation_models::ultra_model::spec(),
    )
    .or_else(resolve_parakeet_dir);
    tracing::info!(dir = ?dir, "speech service Parakeet directory");
    dir
}

fn init_tracing() {
    // rightkit-logs owns the non-panicking, scrubbed stderr writer: a full parent pipe
    // drops a log line instead of aborting the engine. A second init is a no-op.
    let _ = rightkit_logs::install_stderr_tracing("info,rightkit_dictation_runtime=info");
}

/// Read the shell's launch frame from stdin and install the model choice it carries.
///
/// Reads through the process's shared stdin lock, so bytes after the frame (the first request)
/// stay buffered for the IPC loop. Model directories must lie inside the engine's own models
/// root or the shared suite models root, so the frame can choose a model but never point the
/// engine anywhere else.
fn install_launch_frame() -> anyhow::Result<()> {
    use rightkit_dictation_core::engine::launch::{
        EngineLaunch, LAUNCH_SCHEMA, LAUNCH_VERSION, MAX_LAUNCH_BYTES,
    };
    let launch: EngineLaunch = rightkit_job_queue::read_launch_frame(
        &mut std::io::stdin().lock(),
        LAUNCH_SCHEMA,
        LAUNCH_VERSION,
        MAX_LAUNCH_BYTES,
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    let roots: Vec<std::path::PathBuf> = [
        rightkit_dictation_inference::engine_paths::models_base_dir(),
        Some(rightkit_dictation_models::shared::store::paths::models_root()),
    ]
    .into_iter()
    .flatten()
    .collect();
    let inside_roots = |path: &std::path::Path| {
        !path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
            && (path.is_relative() || roots.iter().any(|root| path.starts_with(root)))
    };
    if let Some(dir) = &launch.coreml_model_dir {
        anyhow::ensure!(
            inside_roots(dir),
            "launch frame: model directory outside the models roots"
        );
    }
    if let Some(dir) = &launch.asr_model_subdir {
        anyhow::ensure!(
            inside_roots(std::path::Path::new(dir)),
            "launch frame: model directory outside the models roots"
        );
    }
    tracing::info!(
        backend = %launch.backend,
        load_check = launch.load_check,
        coreml_model_dir = ?launch.coreml_model_dir,
        asr_model_subdir = ?launch.asr_model_subdir,
        "launch frame received"
    );
    rightkit_dictation_inference::engine_paths::install_launch(launch);
    Ok(())
}

fn configure_ort_dylib() -> Result<(), String> {
    use rightkit_onnx::{
        configure_runtime_with_options, CandidateSource, RuntimeBindingOptions, RuntimeCandidate,
    };
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));

    #[cfg(target_os = "windows")]
    let candidates: Vec<RuntimeCandidate> = {
        let exe_sibling = exe_dir
            .as_ref()
            .map(|d| RuntimeCandidate::new(d.join("onnxruntime.dll"), CandidateSource::Bundled));
        let packaged_resource = exe_dir.as_ref().map(|d| {
            RuntimeCandidate::new(
                d.join("resources/runtime/onnxruntime.dll"),
                CandidateSource::Bundled,
            )
        });
        [exe_sibling, packaged_resource]
            .into_iter()
            .flatten()
            .collect()
    };

    #[cfg(target_os = "macos")]
    let candidates: Vec<RuntimeCandidate> = {
        let dylib = "libonnxruntime.dylib";
        let mut paths = Vec::new();
        if let Some(exe_dir) = &exe_dir {
            let bundled =
                |path: std::path::PathBuf| RuntimeCandidate::new(path, CandidateSource::Bundled);
            paths.push(bundled(exe_dir.join("runtime").join(dylib)));
            paths.push(bundled(exe_dir.join("resources/runtime").join(dylib)));
            if let Some(contents) = exe_dir.parent() {
                paths.push(bundled(contents.join("Resources/runtime").join(dylib)));
                paths.push(bundled(
                    contents.join("Resources/resources/runtime").join(dylib),
                ));
            }
        }
        paths
    };

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let candidates: Vec<RuntimeCandidate> = Vec::new();

    // Force-replace: the engine's own binding always wins over anything inherited.
    match configure_runtime_with_options(
        candidates,
        &RuntimeBindingOptions {
            force_replace: true,
            ..RuntimeBindingOptions::default()
        },
    ) {
        Ok(binding) => {
            tracing::info!("ONNX runtime bound: {}", binding.selection.path.display());
        }
        Err(error) => {
            tracing::warn!(reason = %error, "ORT runtime binding failed");
            return Err("Bundled ONNX runtime is missing; repair the app installation.".into());
        }
    }
    Ok(())
}
