//! Native acceptance harness. Uses the production supervisor, loader & store;
//! model fixtures are hardlinked into an explicitly supplied, new scratch root.
//! Never changes the user's installed models. Not included in shipped binaries.
use rightkit_dictation_app_runtime::{
    engine_supervisor::{build_request, EngineSupervisor},
    SupervisorConfig,
};
use rightkit_dictation_core::engine::{EnginePayload, EngineSchemaName};
use rightkit_dictation_models::shared::{self, store};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn check(dir: &Path, audio: &Path) -> Result<serde_json::Value, String> {
    shared::with_load_check(dir, || {
        let (supervisor, _events) = EngineSupervisor::new(false);
        let result = (|| {
            let start = Instant::now();
            supervisor.start()?;
            let info = supervisor.call(
                &build_request(
                    "info",
                    EngineSchemaName::EngineInfo,
                    None,
                    Some(EnginePayload::EngineInfo {
                        mode: "get_info".into(),
                        cold_load_s: None,
                        engine_version: None,
                    }),
                ),
                Duration::from_secs(360),
            )?;
            if let Some(error) = info.error {
                return Err(error.message);
            }
            let load_ms = start.elapsed().as_millis();
            let start = Instant::now();
            let frame = supervisor.call(
                &build_request(
                    "transcribe",
                    EngineSchemaName::FileTranscriptionRequest,
                    None,
                    Some(EnginePayload::FileTranscriptionRequest {
                        path: audio.display().to_string(),
                    }),
                ),
                Duration::from_secs(180),
            )?;
            if let Some(error) = frame.error {
                return Err(error.message);
            }
            let text = match frame.payload {
                Some(EnginePayload::FileTranscriptionResult { text, .. })
                    if !text.trim().is_empty() =>
                {
                    text
                }
                _ => return Err("native transcription returned no text".into()),
            };
            Ok(
                serde_json::json!({"directory":dir, "load_ms":load_ms, "transcribe_ms":start.elapsed().as_millis(), "text":text, "engine_info":info.payload}),
            )
        })();
        supervisor.shutdown();
        result
    })
}

fn fixture(source: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let out = dest.join(entry.file_name());
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            fixture(&entry.path(), &out)?;
        } else {
            std::fs::hard_link(entry.path(), &out)
                .or_else(|_| std::fs::copy(entry.path(), &out).map(|_| ()))
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() < 4 {
        return Err("usage: shared_store_smoke <new-scratch-root> <model-source> <audio.wav> <engine-binary> [fresh|load-only|crash-child]".into());
    }
    let root = PathBuf::from(&args[0]);
    let source = PathBuf::from(&args[1]);
    let audio = PathBuf::from(&args[2]);
    let engine = PathBuf::from(&args[3]);
    // Refuse a stale engine: it must carry the test-mode marker or the suite override is ignored.
    let engine_bytes = std::fs::read(&engine).map_err(|e| format!("read engine binary: {e}"))?;
    if !engine_bytes
        .windows(b"RIGHTKIT-TEST-MODE-COMPILED".len())
        .any(|w| w == b"RIGHTKIT-TEST-MODE-COMPILED")
    {
        return Err(format!(
            "{} lacks the test-mode marker (stale or built without test-mode)",
            engine.display()
        ));
    }
    drop(engine_bytes);
    let child = args.get(4).is_some_and(|mode| mode == "crash-child");
    let fresh = args.get(4).is_some_and(|mode| mode == "fresh");
    if !child {
        std::fs::create_dir(&root).map_err(|e| e.to_string())?;
    }
    let app = root.join("app");
    std::fs::create_dir_all(&app).map_err(|e| e.to_string())?;
    std::env::set_var("RIGHTKIT_APP_DATA_DIR", &app);
    std::env::set_var("RIGHT_SUITE_DIR", root.join("shared"));
    std::env::set_var("RIGHTKIT_ENGINE_BIN", &engine);
    rightkit_dictation_telemetry::install_settings_provider(|| {
        rightkit_dictation_telemetry::TelemetrySettings {
            diagnostics_enabled: true,
            ..Default::default()
        }
    });
    rightkit_dictation_app_runtime::install_supervisor_config_provider(|| SupervisorConfig {
        asr_backend: "parakeet-tdt".into(),
        dictation_language: "auto".into(),
        ..Default::default()
    })
    .map_err(|_| "config provider already set")?;
    let spec = store::catalog::parakeet_spec();
    store::verify_trusted_source(&source, &spec)?;
    let models = root.join("shared/models");
    let legacy = app.join("models");
    let old = legacy.join(shared::legacy_subdir());
    if args.get(4).is_some_and(|mode| mode == "load-only") {
        let report =
            serde_json::json!({"variant":spec.id,"engine":engine,"native":check(&source, &audio)?});
        std::fs::write(
            root.join("report.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .map_err(|e| e.to_string())?;
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
        return Ok(());
    }
    if fresh {
        use rightkit_dictation_models::model_download_grant::{
            acquire_model_grant, ModelGrantRequest,
        };
        assert!(shared::prepare_at(&models, &legacy, &|_| panic!(
            "empty install loaded a model"
        ))?
        .is_none());
        let platform = if cfg!(target_os = "macos") {
            "macos"
        } else {
            "windows"
        };
        let route = rightkit_dictation_models::ultra_model::ULTRA_ROUTE.to_string();
        let device = format!("rightkit-native-acceptance-{}", std::process::id());
        let agent = shared::http_client();
        let token = acquire_model_grant(
            ModelGrantRequest {
                worker_base: store::catalog::MODEL_DOWNLOAD_BASE,
                route: &route,
                model: "parakeet-tdt",
                platform,
                sha256: spec.version,
                device_id: &device,
            },
            std::time::Duration::from_secs(3600),
        )?;
        let authorization = format!("Bearer {token}");
        store::ensure_ready(
            &agent,
            &models,
            &spec,
            &store::EnsureOptions {
                seed_dir: None,
                authorization: Some(&authorization),
                app_id: shared::APP_ID,
            },
            |_| {},
        )?;
        let measurements = std::cell::RefCell::new(None);
        let destination = shared::prepare_at(&models, &legacy, &|dir| {
            *measurements.borrow_mut() = Some(check(dir, &audio)?);
            Ok(())
        })?
        .ok_or("fresh download did not activate")?;
        let report = serde_json::json!({"fresh_download":true,"variant":spec.id,"revision":spec.version,"descriptor":store::parakeet_variant_descriptor(),"manifest":spec.expected_manifest(),"engine":engine,"directory":destination,"native":measurements.into_inner()});
        std::fs::write(
            root.join("report.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .map_err(|e| e.to_string())?;
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
        return Ok(());
    }
    if child {
        shared::prepare_at(&models, &legacy, &|_| {
            std::fs::write(root.join("kill-ready"), b"destination published")
                .map_err(|e| e.to_string())?;
            std::thread::park();
            Err("child unexpectedly resumed".into())
        })?;
        return Err("crash child did not reach load check".into());
    }
    fixture(&source, &old)?;
    let before = check(&old, &audio)?;
    // Kill an actual process after publish & before native load/retirement.
    let mut crash = std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
        .args(&args)
        .arg("crash-child")
        .spawn()
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    while !root.join("kill-ready").is_file() {
        if let Some(status) = crash.try_wait().map_err(|e| e.to_string())? {
            return Err(format!("crash child exited early: {status}"));
        }
        if started.elapsed() > Duration::from_secs(120) {
            let _ = crash.kill();
            let _ = crash.wait();
            return Err("crash checkpoint timeout".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    crash.kill().map_err(|e| e.to_string())?;
    crash.wait().map_err(|e| e.to_string())?;
    let load = std::cell::RefCell::new(Vec::new());
    let destination = shared::prepare_at(&models, &legacy, &|dir| {
        load.borrow_mut().push(check(dir, &audio)?);
        Ok(())
    })?
    .ok_or("migration did not activate")?;
    store::verify_trusted_source(&old, &spec)?;
    let after = check(&destination, &audio)?;
    if before["text"] != after["text"] {
        return Err("transcript changed after migration".into());
    }
    // HR-before-SR & SR-before-HR both reuse this exact byte-verified variant.
    store::ensure_ready(
        &shared::http_client(),
        &models,
        &spec,
        &store::EnsureOptions {
            seed_dir: None,
            authorization: None,
            app_id: "scraperight",
        },
        |_| {},
    )?;
    let peer = shared::prepare_at(&models, &root.join("new-app/models"), &|dir| {
        check(dir, &audio).map(|_| ())
    })?
    .ok_or("peer reuse failed")?;
    shared::unregister()?;
    assert!(models.join("refs/scraperight.json").is_file());
    store::verify_trusted_source(&destination, &spec)?;
    let report = serde_json::json!({"variant":spec.id,"revision":spec.version,"descriptor":store::parakeet_variant_descriptor(),"manifest":spec.expected_manifest(),"engine":engine,"before":before,"recovery_loads":load.into_inner(),"after":after,"peer_directory":peer,"kill_restart":true,"legacy_readable":true,"uninstall_preserves_peer":true});
    std::fs::write(
        root.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    Ok(())
}
/// Fail closed before any filesystem access. This harness always overrides the suite root
/// (RIGHT_SUITE_DIR below); without `test-mode` rightkit-daemon would ignore that and use the real
/// user data folder, so refuse to start. Also refuses an externally set override, as the engine bin does.
fn refuse_ignored_suite_override() {
    if rightkit_daemon::mode::TEST_CAPABILITY {
        return;
    }
    eprintln!("refusing to start: shared_store_smoke sets the suite root but this build has no test-mode (rightkit-daemon would ignore it and use the real user data folder); build with --features heardright-app-runtime/test-mode");
    std::process::exit(78);
}
fn main() {
    refuse_ignored_suite_override();
    if let Err(error) = run() {
        eprintln!("native shared-store smoke failed: {error}");
        std::thread::sleep(Duration::from_millis(300));
        std::process::exit(1);
    }
}
