//! Windows engine commit probe (bogusyogi/#296).
//!
//! Loads the same sessions in the same order as the resident `rightkit-dictation-engine`
//! and prints Private Bytes, Working Set and commit (PagefileUsage) before and
//! after each load, so the stage that leaves ~1.4 GB of committed-but-untouched
//! pages behind is visible. It reuses the engine's own load entry points
//! (`init_windows_ort`, `AsrRuntime::load`, `SpeechVad`, `AsrRuntime::load_probe`);
//! the only session built here by hand is the optional `--raw-encoder` A/B arm,
//! which exists to prove a candidate ORT option before it is patched in.
//!
//! Engine order (rightkit-dictation-runtime entry.rs / worker_main.rs / unified_probe_lane.rs):
//!   1. bind ORT runtime, `init_windows_ort()` (shared global CPU pool environment)
//!   2. `AsrRuntime::load(models, AsrEp::resolve_default())` on `hr-main-asr-worker`
//!   3. `warm_main_asr` (one second of silence through `with_inference_lease`)
//!   4. `SpeechVad::new()` (Silero, own loader thread)
//!   5. `AsrRuntime::load_probe` (Sherpa KWS cascade, own bundled onnxruntime.dll)
//!
//! usage (one process per arm, so each arm starts from a cold heap):
//!   windows_engine_commit_probe [--models DIR] [--ep auto|dml|cpu] [--wav FILE]
//!       [--legacy-env] [--ort PATH] [--vad-model FILE] [--kws-dir DIR]
//!       [--skip-vad] [--skip-kws] [--settle-ms N] [--hold-secs N]
//!   windows_engine_commit_probe --raw-encoder baseline|device-init|no-arena|both
//!       [--encoder FILE] [--device N] [--settle-ms N]
//!
//! `--legacy-env` reproduces the windows_asr_bench environment: no runtime
//! binding and no `init_windows_ort()`, so ORT_DYLIB_PATH must be set and the
//! sessions use per-session pools.

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("windows_engine_commit_probe is Windows-only");
}

#[cfg(target_os = "windows")]
fn main() {
    probe::run();
}

#[cfg(target_os = "windows")]
mod probe {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use rightkit_dictation_inference::asr::{AsrEp, AsrRuntime};

    const MB: f64 = 1024.0 * 1024.0;

    // Hand-declared kernel32 imports: windows-sys in this crate does not enable
    // Win32_System_ProcessStatus / LibraryLoader, and no Cargo.toml edit is made.
    #[repr(C)]
    #[allow(non_snake_case)]
    struct ProcessMemoryCountersEx {
        cb: u32,
        PageFaultCount: u32,
        PeakWorkingSetSize: usize,
        WorkingSetSize: usize,
        QuotaPeakPagedPoolUsage: usize,
        QuotaPagedPoolUsage: usize,
        QuotaPeakNonPagedPoolUsage: usize,
        QuotaNonPagedPoolUsage: usize,
        PagefileUsage: usize,
        PeakPagefileUsage: usize,
        PrivateUsage: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(
            process: isize,
            counters: *mut ProcessMemoryCountersEx,
            cb: u32,
        ) -> i32;
        fn GetModuleHandleW(name: *const u16) -> isize;
        fn GetModuleFileNameW(module: isize, buffer: *mut u16, size: u32) -> u32;
    }

    #[derive(Clone, Copy, Default)]
    struct Sample {
        private: usize,
        working_set: usize,
        commit: usize,
    }

    fn sample() -> Sample {
        let mut counters = ProcessMemoryCountersEx {
            cb: std::mem::size_of::<ProcessMemoryCountersEx>() as u32,
            PageFaultCount: 0,
            PeakWorkingSetSize: 0,
            WorkingSetSize: 0,
            QuotaPeakPagedPoolUsage: 0,
            QuotaPagedPoolUsage: 0,
            QuotaPeakNonPagedPoolUsage: 0,
            QuotaNonPagedPoolUsage: 0,
            PagefileUsage: 0,
            PeakPagefileUsage: 0,
            PrivateUsage: 0,
        };
        let ok =
            unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
        if ok == 0 {
            return Sample::default();
        }
        Sample {
            private: counters.PrivateUsage,
            working_set: counters.WorkingSetSize,
            commit: counters.PagefileUsage,
        }
    }

    static PEAK_PRIVATE: AtomicUsize = AtomicUsize::new(0);
    static PEAK_WS: AtomicUsize = AtomicUsize::new(0);

    /// 100 ms sampler, the same cadence the engine dictation sampler used.
    fn spawn_sampler() {
        std::thread::Builder::new()
            .name("probe-sampler".into())
            .spawn(|| loop {
                let s = sample();
                PEAK_PRIVATE.fetch_max(s.private, Ordering::Relaxed);
                PEAK_WS.fetch_max(s.working_set, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(100));
            })
            .expect("spawn sampler");
    }

    fn mark(label: &str, settle: Duration) {
        let now = sample();
        PEAK_PRIVATE.fetch_max(now.private, Ordering::Relaxed);
        PEAK_WS.fetch_max(now.working_set, Ordering::Relaxed);
        let peak_private = PEAK_PRIVATE.swap(0, Ordering::Relaxed);
        let peak_ws = PEAK_WS.swap(0, Ordering::Relaxed);
        println!(
            "STAGE {label:<40} private={:>8.1} MB  ws={:>8.1} MB  commit={:>8.1} MB  | peak_since_last private={:>8.1} MB ws={:>8.1} MB",
            now.private as f64 / MB,
            now.working_set as f64 / MB,
            now.commit as f64 / MB,
            peak_private as f64 / MB,
            peak_ws as f64 / MB,
        );
        if !settle.is_zero() {
            std::thread::sleep(settle);
            let after = sample();
            println!(
                "STAGE {:<40} private={:>8.1} MB  ws={:>8.1} MB  commit={:>8.1} MB",
                format!("{label} +{}ms", settle.as_millis()),
                after.private as f64 / MB,
                after.working_set as f64 / MB,
                after.commit as f64 / MB,
            );
        }
    }

    fn module_path(name: &str) -> Option<PathBuf> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe { GetModuleHandleW(wide.as_ptr()) };
        if handle == 0 {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let len = unsafe { GetModuleFileNameW(handle, buffer.as_mut_ptr(), buffer.len() as u32) };
        (len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buffer[..len as usize])))
    }

    fn print_modules(label: &str) {
        for name in [
            "onnxruntime.dll",
            "DirectML.dll",
            "sherpa-onnx-c-api.dll",
            "whisper.dll",
        ] {
            match module_path(name) {
                Some(path) => {
                    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    println!(
                        "MODULE [{label}] {name} -> {} ({size} bytes)",
                        path.display()
                    );
                }
                None => println!("MODULE [{label}] {name} -> not loaded"),
            }
        }
    }

    fn print_env(label: &str) {
        for key in [
            "RIGHTKIT_DML_DEVICE_ID",
            "RIGHTKIT_APP_DATA_DIR",
            "RIGHTKIT_MODELS_DIR",
            "RIGHTKIT_ASR_BACKEND",
            "RIGHTKIT_DICTATION_LANG",
            "RIGHTKIT_SHERPA_KWS_DIR",
            "RIGHTKIT_VAD_MODEL",
            "ORT_DYLIB_PATH",
        ] {
            println!(
                "ENV [{label}] {key}={}",
                std::env::var(key).unwrap_or_else(|_| "<unset>".into())
            );
        }
    }

    #[derive(Default)]
    struct Args {
        models: Option<PathBuf>,
        ep: String,
        wav: Option<PathBuf>,
        legacy_env: bool,
        ort: Option<PathBuf>,
        vad_model: Option<PathBuf>,
        kws_dir: Option<PathBuf>,
        skip_vad: bool,
        skip_kws: bool,
        settle_ms: u64,
        hold_secs: u64,
        raw_encoder: Option<String>,
        encoder: Option<PathBuf>,
        device: Option<i32>,
    }

    fn parse_args() -> Result<Args, String> {
        let mut args = Args {
            ep: "auto".into(),
            settle_ms: 2000,
            ..Args::default()
        };
        let mut it = std::env::args().skip(1);
        while let Some(flag) = it.next() {
            let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
            match flag.as_str() {
                "--models" => args.models = Some(value("--models")?.into()),
                "--ep" => args.ep = value("--ep")?,
                "--wav" => args.wav = Some(value("--wav")?.into()),
                "--legacy-env" => args.legacy_env = true,
                "--ort" => args.ort = Some(value("--ort")?.into()),
                "--vad-model" => args.vad_model = Some(value("--vad-model")?.into()),
                "--kws-dir" => args.kws_dir = Some(value("--kws-dir")?.into()),
                "--skip-vad" => args.skip_vad = true,
                "--skip-kws" => args.skip_kws = true,
                "--settle-ms" => {
                    args.settle_ms = value("--settle-ms")?
                        .parse()
                        .map_err(|e| format!("--settle-ms: {e}"))?
                }
                "--hold-secs" => {
                    args.hold_secs = value("--hold-secs")?
                        .parse()
                        .map_err(|e| format!("--hold-secs: {e}"))?
                }
                "--raw-encoder" => args.raw_encoder = Some(value("--raw-encoder")?),
                "--encoder" => args.encoder = Some(value("--encoder")?.into()),
                "--device" => {
                    args.device = Some(
                        value("--device")?
                            .parse()
                            .map_err(|e| format!("--device: {e}"))?,
                    )
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }
        Ok(args)
    }

    fn repo_resource(relative: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../src-tauri/resources")
            .join(relative)
    }

    /// Mirrors rightkit-dictation-runtime entry.rs `configure_ort_dylib` (force-replace
    /// binding) followed by `init_windows_ort`, unless `--legacy-env`.
    fn init_environment(args: &Args) -> Result<(), String> {
        if args.legacy_env {
            if std::env::var_os("ORT_DYLIB_PATH").is_none() {
                return Err("--legacy-env needs ORT_DYLIB_PATH (bench environment)".into());
            }
            println!("ENVMODE legacy (bench): no runtime binding, no init_windows_ort");
            return Ok(());
        }
        use rightkit_onnx::{
            configure_runtime_with_options, CandidateSource, RuntimeBindingOptions,
            RuntimeCandidate,
        };
        let mut candidates = Vec::new();
        if let Some(path) = &args.ort {
            candidates.push(RuntimeCandidate::new(
                path.clone(),
                CandidateSource::Explicit,
            ));
        }
        if let Some(path) = std::env::var_os("ORT_DYLIB_PATH")
            .map(PathBuf::from)
            .filter(|p| p.is_file())
        {
            candidates.push(RuntimeCandidate::new(path, CandidateSource::Explicit));
        }
        let bundled = repo_resource("runtime/onnxruntime.dll");
        if let Ok(path) = std::fs::canonicalize(&bundled) {
            candidates.push(RuntimeCandidate::new(path, CandidateSource::Explicit));
        }
        let binding = configure_runtime_with_options(
            candidates,
            &RuntimeBindingOptions {
                force_replace: true,
                ..RuntimeBindingOptions::default()
            },
        )
        .map_err(|e| format!("bind ORT runtime: {e}"))?;
        println!(
            "ENVMODE production: ORT runtime bound to {}",
            binding.selection.path.display()
        );
        rightkit_dictation_inference::init_windows_ort()
            .map_err(|e| format!("init_windows_ort: {e}"))
    }

    fn find_encoder(models: &Path) -> Option<PathBuf> {
        let direct = |dir: &Path| {
            [
                "encoder-model.int8.onnx",
                "encoder-model.onnx",
                "encoder.onnx",
            ]
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.is_file())
        };
        direct(models).or_else(|| {
            std::fs::read_dir(models)
                .ok()?
                .flatten()
                .filter_map(|entry| direct(&entry.path()))
                .next()
        })
    }

    /// A/B arm only: the encoder session exactly as heardright-onnx-asr
    /// `build_with_pool` builds it for static + DirectML, plus optional options
    /// under test. Not a replacement for the engine load path.
    #[derive(Clone, Copy, Default)]
    struct RawVariant {
        device_init: bool,
        no_arena: bool,
        /// GraphOptimizationLevel::Disable (no fp16 Cast constant folding).
        opt_disable: bool,
        /// session.disable_prepacking=1.
        no_prepack: bool,
        /// enable_mem_pattern off.
        no_mempattern: bool,
        /// CPU EP only (no DirectML): is the commit DML-side?
        cpu_only: bool,
    }

    fn build_raw_encoder(
        path: &Path,
        device: Option<i32>,
        v: RawVariant,
    ) -> Result<rightkit_onnx::ort::session::Session, String> {
        use rightkit_onnx::ort;
        let environment =
            rightkit_onnx::ort_environment().map_err(|e| format!("ort environment: {e}"))?;
        let level = if v.opt_disable {
            ort::session::builder::GraphOptimizationLevel::Disable
        } else {
            ort::session::builder::GraphOptimizationLevel::Level1
        };
        let mut builder = ort::session::Session::builder(environment)
            .map_err(|e| format!("session builder: {e}"))?
            .with_independent_thread_pool()
            .map_err(|e| format!("independent pool: {e}"))?
            .with_config_entry("ep.dml.disable_graph_fusion", "1")
            .map_err(|e| format!("disable_graph_fusion: {e}"))?
            .with_optimization_level(level)
            .map_err(|e| format!("optimization level: {e}"))?;
        if v.no_prepack {
            builder = builder
                .with_config_entry("session.disable_prepacking", "1")
                .map_err(|e| format!("disable_prepacking: {e}"))?;
        }
        if v.no_mempattern {
            builder = builder
                .with_memory_pattern(false)
                .map_err(|e| format!("memory pattern: {e}"))?;
        }
        let device_init = v.device_init;
        let no_arena = v.no_arena;
        if device_init {
            builder = builder
                .with_device_allocated_initializers()
                .map_err(|e| format!("device-allocated initializers: {e}"))?;
        }
        let mut dml = ort::ep::DirectML::default();
        if let Some(id) = device {
            dml = dml.with_device_id(id);
        }
        // DirectML first: ORT assigns nodes in list order, CPU stays the fallback.
        let mut providers = if v.cpu_only {
            Vec::new()
        } else {
            vec![dml.build().error_on_failure()]
        };
        if no_arena || v.cpu_only {
            providers.push(ort::ep::CPU::default().with_arena_allocator(false).build());
        }
        builder = builder
            .with_execution_providers(providers)
            .map_err(|e| format!("execution providers: {e}"))?;
        builder
            .commit_from_file(path)
            .map_err(|e| format!("load {}: {e}", path.display()))
    }

    fn run_raw_encoder(args: &Args, variant: &str) -> Result<(), String> {
        let v = match variant {
            "baseline" => RawVariant::default(),
            "device-init" => RawVariant { device_init: true, ..Default::default() },
            "no-arena" => RawVariant { no_arena: true, ..Default::default() },
            "both" => RawVariant { device_init: true, no_arena: true, ..Default::default() },
            "opt-disable" => RawVariant { opt_disable: true, ..Default::default() },
            "no-prepack" => RawVariant { no_prepack: true, ..Default::default() },
            "no-mempattern" => RawVariant { no_mempattern: true, ..Default::default() },
            "cpu-only" => RawVariant { cpu_only: true, ..Default::default() },
            other => return Err(format!("--raw-encoder {other}: expected baseline|device-init|no-arena|both|opt-disable|no-prepack|no-mempattern|cpu-only")),
        };
        let models = args
            .models
            .clone()
            .unwrap_or_else(|| rightkit_dictation_inference::asr::models_base(None));
        let encoder = args
            .encoder
            .clone()
            .or_else(|| find_encoder(&models))
            .ok_or_else(|| {
                format!(
                    "no encoder model under {} (use --encoder)",
                    models.display()
                )
            })?;
        println!(
            "RAW encoder={} variant={variant} device={:?}",
            encoder.display(),
            args.device
        );
        let settle = Duration::from_millis(args.settle_ms);
        mark("raw: baseline", Duration::ZERO);
        init_environment(args)?;
        mark("raw: environment ready", Duration::ZERO);
        let started = Instant::now();
        let session = build_raw_encoder(&encoder, args.device, v)?;
        println!("RAW build_ms={}", started.elapsed().as_millis());
        print_modules("raw after build");
        mark(&format!("raw[{variant}]: encoder built"), settle);
        drop(session);
        mark(&format!("raw[{variant}]: encoder dropped"), settle);
        Ok(())
    }

    fn first_decode(model: &mut AsrRuntime, wav: Option<&Path>) {
        let silence = vec![0.0f32; 16_000];
        let started = Instant::now();
        let warm = model.with_inference_lease("probe_warmup", |model| {
            rightkit_dictation_inference::asr::apply_utterance_bias(model);
            model.transcribe_under_lease(&silence).map(|_| ())
        });
        println!(
            "DECODE warmup ok={} ms={}",
            warm.is_ok(),
            started.elapsed().as_millis()
        );
        if let Err(error) = warm {
            println!("DECODE warmup error: {error}");
        }
        let Some(path) = wav else { return };
        match rightkit_audio::wav::read_wav_mono_f32(path) {
            Ok(wav) if wav.sample_rate == 16_000 => {
                let started = Instant::now();
                let result = model.with_inference_lease("probe_decode", |model| {
                    model.transcribe_final_result_under_lease(&wav.samples)
                });
                match result {
                    Ok(result) => println!(
                        "DECODE wav={} ms={} text={:?}",
                        path.display(),
                        started.elapsed().as_millis(),
                        result.text
                    ),
                    Err(error) => println!("DECODE wav={} error: {error}", path.display()),
                }
            }
            Ok(wav) => println!(
                "DECODE wav skipped: expected 16 kHz mono, got {} Hz",
                wav.sample_rate
            ),
            Err(error) => println!("DECODE wav read failed: {error}"),
        }
    }

    fn run_engine_order(args: &Args) -> Result<(), String> {
        let settle = Duration::from_millis(args.settle_ms);
        let ep = match args.ep.as_str() {
            "auto" => AsrEp::resolve_default(),
            "dml" => AsrEp::Dml,
            "cpu" => AsrEp::Cpu,
            other => return Err(format!("--ep {other}: expected auto|dml|cpu")),
        };
        let models = args
            .models
            .clone()
            .unwrap_or_else(|| rightkit_dictation_inference::asr::models_base(None));
        println!("CONFIG ep={} models={}", ep.as_str(), models.display());
        print_env("start");

        mark("0 baseline", Duration::ZERO);
        init_environment(args)?;
        mark("1 ort environment ready", Duration::ZERO);

        let started = Instant::now();
        let mut model = AsrRuntime::load(&models, ep)?;
        println!("LOAD asr load_ms={}", started.elapsed().as_millis());
        print_env("after asr load");
        print_modules("after asr load");
        mark("2 AsrRuntime::load (mel+encoder+decoder)", settle);

        first_decode(&mut model, args.wav.as_deref());
        mark("3 first decode (warm_main_asr)", settle);

        let _vad = if args.skip_vad {
            None
        } else {
            let path = args
                .vad_model
                .clone()
                .or_else(|| std::env::var_os("RIGHTKIT_VAD_MODEL").map(PathBuf::from))
                .unwrap_or_else(|| repo_resource("vad/silero_vad_16k_op15.onnx"));
            let mut vad = rightkit_dictation_inference::vad::SpeechVad::with_model_path(Some(path));
            let deadline = Instant::now() + Duration::from_secs(10);
            while vad.status() == rightkit_dictation_inference::vad::VadStatus::Loading
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            println!(
                "LOAD vad status_ready={}",
                vad.status() == rightkit_dictation_inference::vad::VadStatus::Ready
            );
            mark("4 SpeechVad (Silero)", settle);
            Some(vad)
        };

        let _kws = if args.skip_kws {
            None
        } else {
            let dir = args.kws_dir.clone().unwrap_or_else(|| repo_resource("kws"));
            if std::env::var_os("RIGHTKIT_SHERPA_KWS_DIR").is_none() {
                std::env::set_var("RIGHTKIT_SHERPA_KWS_DIR", &dir);
            }
            let started = Instant::now();
            let kws = AsrRuntime::load_probe(&models, ep);
            println!(
                "LOAD kws ok={} load_ms={}",
                kws.is_ok(),
                started.elapsed().as_millis()
            );
            match kws {
                Ok(mut kws) => {
                    let reset = kws.reset_probe_stream();
                    println!("LOAD kws reset_ok={}", reset.is_ok());
                    print_modules("after kws load");
                    mark("5 load_probe (Sherpa KWS)", settle);
                    Some(kws)
                }
                Err(error) => {
                    println!("LOAD kws error: {error}");
                    None
                }
            }
        };

        std::thread::sleep(Duration::from_secs(3));
        mark("6 final (all resident)", Duration::ZERO);
        println!(
            "DONE pid={} holding {} s for external samplers",
            std::process::id(),
            args.hold_secs
        );
        std::thread::sleep(Duration::from_secs(args.hold_secs));
        drop(model);
        Ok(())
    }

    pub fn run() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .with_ansi(false)
            .without_time()
            .with_writer(std::io::stderr)
            .try_init();
        let args = match parse_args() {
            Ok(args) => args,
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(2);
            }
        };
        spawn_sampler();
        // The engine loads ASR on the `hr-main-asr-worker` thread, not main.
        let worker = std::thread::Builder::new()
            .name("hr-main-asr-worker".into())
            .spawn(move || match args.raw_encoder.clone() {
                Some(variant) => run_raw_encoder(&args, &variant),
                None => run_engine_order(&args),
            })
            .expect("spawn probe worker");
        match worker.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                eprintln!("probe failed: {error}");
                std::process::exit(1);
            }
            Err(_) => {
                eprintln!("probe worker panicked");
                std::process::exit(1);
            }
        }
    }
}
