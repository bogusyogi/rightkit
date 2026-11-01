//! Configurable, serial Windows production-entry parameter probe.
//! Inputs contain PCM paths/hashes only; references are scorer-private.
//! Session groups load sequentially. Policy arms rotate within each clip.
//! Output is append-only JSONL, excluding file IO/load/warmup from decode time.

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("Windows only");
    std::process::exit(2);
}

#[cfg(target_os = "windows")]
mod windows {
    use rightkit_dictation_inference::asr::{AsrEp, AsrRuntime};
    use serde::{Deserialize, Serialize};
    use std::collections::{BTreeMap, HashSet};
    use std::fs::{self, OpenOptions};
    use std::io::{BufWriter, Write};
    use std::path::Path;
    use std::time::Instant;

    const KEYS: &[&str] = &[
        "RIGHTKIT_ASR_BACKEND",
        "RIGHTKIT_ASR_LANG",
        "RIGHTKIT_DICTATION_LANG",
        "RIGHTKIT_ASR_BEAM_WIDTH",
        "RIGHTKIT_ASR_BEAM_EMIT_REWARD",
        "RIGHTKIT_ASR_CONTEXT_BIAS_SCORE",
        "RIGHTKIT_DISABLE_ASR_CONTEXT_BIAS",
        "RIGHTKIT_TDT_DURATION_CAP",
        "RIGHTKIT_TDT_PRUNING",
        "RIGHTKIT_RNNT_MAX_TOKENS",
        "RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT",
        "RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT_DIR",
        "RIGHTKIT_ONNX_CPU_THREADS_EXPERIMENT",
        "RIGHTKIT_ONNX_MEL_THREADS_EXPERIMENT",
        "RIGHTKIT_ONNX_DECODER_THREADS_EXPERIMENT",
        "RIGHTKIT_ONNX_CPU_SPIN_EXPERIMENT",
        "RIGHTKIT_ONNX_PREDICTOR_THREADS_EXPERIMENT",
        "RIGHTKIT_ONNX_JOINT_THREADS_EXPERIMENT",
        "RIGHTKIT_ONNX_CPU_OPT_EXPERIMENT",
        "RIGHTKIT_ONNX_CPU_MEMORY_PATTERN_EXPERIMENT",
        "RIGHTKIT_ONNX_CPU_ARENA_EXPERIMENT",
        "RIGHTKIT_ONNX_CPU_PARALLEL_EXPERIMENT",
        "RIGHTKIT_ONNX_CPU_INTER_THREADS_EXPERIMENT",
        "RIGHTKIT_ASR_ALLOW_DML_FUSION",
        "RIGHTKIT_ONNX_CPU_ENCODER_SHARED_EXPERIMENT",
    ];

    #[derive(Deserialize)]
    struct Input {
        id: String,
        path: String,
        sha256: String,
        duration_s: f64,
    }
    #[derive(Deserialize, Serialize)]
    struct Arm {
        label: String,
        #[serde(default)]
        env: BTreeMap<String, String>,
    }
    #[derive(Deserialize, Serialize)]
    struct Group {
        label: String,
        #[serde(default)]
        env: BTreeMap<String, String>,
        arms: Vec<Arm>,
    }
    #[derive(Deserialize, Serialize)]
    struct GlobalPool {
        threads: usize,
        spin: bool,
        #[serde(default = "one")]
        inter_threads: usize,
        #[serde(default)]
        encoder_shared: bool,
    }
    fn one() -> usize {
        1
    }
    #[derive(Deserialize)]
    struct Plan {
        manifest_sha256: String,
        expected_clips: usize,
        groups: Vec<Group>,
        #[serde(default)]
        global_pool: Option<GlobalPool>,
        #[serde(default)]
        production_environment: bool,
        #[serde(default)]
        vad_order: Option<String>,
        #[serde(default = "dml")]
        provider: String,
        #[serde(default)]
        startup_order: Option<String>,
        #[serde(default)]
        whisper_first: bool,
        #[serde(default)]
        condition_audio: bool,
    }
    fn dml() -> String {
        "dml".into()
    }
    #[derive(Serialize)]
    struct Token {
        text: String,
        start: f32,
        end: f32,
    }
    #[derive(Serialize)]
    struct Row<'a> {
        event: &'static str,
        group: &'a str,
        arm: &'a str,
        id: &'a str,
        manifest_index: usize,
        order_index: usize,
        audio_seconds: f64,
        elapsed_ms: f64,
        conditioning_ms: f64,
        cpu_user_ms: f64,
        cpu_kernel_ms: f64,
        text: Option<String>,
        tokens: Vec<Token>,
        control_intent: Option<String>,
        error: Option<String>,
    }

    // Process-wide CPU includes every ORT worker, unlike a calling-thread clock.
    fn process_cpu_ms() -> Result<(f64, f64), String> {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let mut creation: FILETIME = unsafe { std::mem::zeroed() };
        let mut exit: FILETIME = unsafe { std::mem::zeroed() };
        let mut kernel: FILETIME = unsafe { std::mem::zeroed() };
        let mut user: FILETIME = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        };
        if ok == 0 {
            return Err(format!(
                "process CPU clock: {}",
                std::io::Error::last_os_error()
            ));
        }
        let ms = |value: FILETIME| {
            ((u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)) as f64
                / 10_000.0
        };
        Ok((ms(user), ms(kernel)))
    }

    fn hash(bytes: &[u8]) -> String {
        rightkit_fs::digest::sha256_bytes(bytes)
    }
    fn emit<T: Serialize>(out: &mut BufWriter<std::fs::File>, value: &T) -> Result<(), String> {
        serde_json::to_writer(&mut *out, value).map_err(|e| e.to_string())?;
        out.write_all(b"\n").map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())
    }
    fn settings(group: &Group, arm: &Arm) {
        for key in KEYS {
            std::env::remove_var(key);
        }
        for (key, value) in [
            ("RIGHTKIT_ASR_BACKEND", "parakeet-tdt"),
            ("RIGHTKIT_DICTATION_LANG", "auto"),
            ("RIGHTKIT_ASR_BEAM_WIDTH", "2"),
            ("RIGHTKIT_ASR_BEAM_EMIT_REWARD", "0.10"),
            ("RIGHTKIT_TDT_DURATION_CAP", "2"),
            ("RIGHTKIT_TDT_PRUNING", "global"),
            ("RIGHTKIT_RNNT_MAX_TOKENS", "10"),
            ("RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT", "fused"),
        ] {
            std::env::set_var(key, value);
        }
        for (key, value) in group.env.iter().chain(arm.env.iter()) {
            std::env::set_var(key, value);
        }
        std::env::set_var("RIGHTKIT_PREDICTOR_BENCH", "1");
    }
    fn wav(path: &Path) -> Result<Vec<f32>, String> {
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
    fn vad_smoke() -> Result<rightkit_dictation_inference::vad::SpeechVad, String> {
        use rightkit_dictation_inference::vad::{SpeechVad, VadStatus};
        let mut vad = SpeechVad::new();
        let deadline = Instant::now() + std::time::Duration::from_secs(6);
        while vad.status() == VadStatus::Loading && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if vad.status() != VadStatus::Ready {
            return Err("production VAD not ready".into());
        }
        let silence = vad
            .observe_frames(&[0.0; 512 * 10])
            .ok_or("VAD silence inference failed")?;
        if silence.len() != 10 || silence.iter().any(|speech| *speech) {
            return Err("VAD silence verdict mismatch".into());
        }
        Ok(vad)
    }
    fn run() -> Result<(), String> {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("info")
            .with_writer(std::io::stderr)
            .try_init();
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.len() != 4 {
            return Err(
                "usage: windows_decode_probe <model> <manifest> <plan> <output.jsonl>".into(),
            );
        }
        let manifest_bytes = fs::read(&args[1]).map_err(|e| e.to_string())?;
        let plan_bytes = fs::read(&args[2]).map_err(|e| e.to_string())?;
        let plan: Plan = serde_json::from_slice(&plan_bytes).map_err(|e| e.to_string())?;
        let provider = match plan.provider.as_str() {
            "dml" => AsrEp::Dml,
            "cpu" => AsrEp::Cpu,
            other => return Err(format!("invalid provider: {other}")),
        };
        let inputs: Vec<Input> =
            serde_json::from_slice(&manifest_bytes).map_err(|e| e.to_string())?;
        if hash(&manifest_bytes) != plan.manifest_sha256 || inputs.len() != plan.expected_clips {
            return Err("manifest hash/count mismatch".into());
        }
        let ids: HashSet<&str> = inputs.iter().map(|v| v.id.as_str()).collect();
        if inputs.is_empty() || ids.len() != inputs.len() || plan.groups.is_empty() {
            return Err("empty/duplicate manifest or empty groups".into());
        }
        let mut labels = HashSet::new();
        for group in &plan.groups {
            if !labels.insert(group.label.as_str()) || group.arms.is_empty() {
                return Err("empty/duplicate group".into());
            }
            let mut arm_labels = HashSet::new();
            for arm in &group.arms {
                if !arm_labels.insert(arm.label.as_str()) {
                    return Err("duplicate arm".into());
                }
                for key in group.env.keys().chain(arm.env.keys()) {
                    if !KEYS.contains(&key.as_str()) || key == "RIGHTKIT_ASR_ALLOW_DML_FUSION" {
                        return Err(format!("unsupported experiment key: {key}"));
                    }
                    if plan.global_pool.is_some()
                        && (key.contains("THREADS_EXPERIMENT") || key.contains("CPU_SPIN"))
                        && !((key == "RIGHTKIT_ONNX_PREDICTOR_THREADS_EXPERIMENT"
                            || key == "RIGHTKIT_ONNX_JOINT_THREADS_EXPERIMENT")
                            && group.env.get(key).map(String::as_str) == Some("1"))
                    {
                        return Err(
                            "global pool cannot be combined with per-session pool overrides".into(),
                        );
                    }
                }
                // CPU session settings must be group-scoped so they take
                // effect before model load, never just appear in metadata.
                if arm.env.keys().any(|key| {
                    key.contains("THREADS_EXPERIMENT")
                        || key.contains("CONTEXT_BIAS")
                        || key.contains("CPU_SPIN")
                        || key.contains("CPU_OPT")
                        || key.contains("CPU_MEMORY")
                        || key.contains("CPU_ARENA")
                        || key.contains("CPU_PARALLEL")
                        || key == "RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT_DIR"
                }) {
                    return Err("session settings must be group-scoped".into());
                }
            }
        }
        // ORT can configure its pool only once. Old plans explicitly preserve
        // historical defaults; production plans exercise library initialization.
        std::env::remove_var("RIGHTKIT_ONNX_GLOBAL_POOL_ACTIVE_EXPERIMENT");
        std::env::remove_var("RIGHTKIT_ONNX_DML_SHARED_POOL_EXPERIMENT");
        std::env::remove_var("RIGHTKIT_ONNX_LEGACY_ENV_EXPERIMENT");
        if plan.production_environment && plan.global_pool.is_some() {
            return Err("production environment cannot override global pool".into());
        }
        if plan
            .vad_order
            .as_deref()
            .is_some_and(|order| !matches!(order, "before_asr" | "after_asr"))
        {
            return Err("invalid VAD load order".into());
        }
        if !plan.production_environment && plan.global_pool.is_none() {
            std::env::set_var("RIGHTKIT_ONNX_LEGACY_ENV_EXPERIMENT", "1");
        }
        if let Some(pool) = &plan.global_pool {
            if !(1..=64).contains(&pool.threads) || !(1..=64).contains(&pool.inter_threads) {
                return Err("invalid global pool size".into());
            }
            // Deferred commit of the shared pool only; telemetry left at ort's default, as before.
            let selection = rightkit_onnx::RuntimeSelection {
                path: rightkit_onnx::configured_runtime()
                    .ok_or("ORT runtime path is not configured")?,
                source: rightkit_onnx::CandidateSource::Explicit,
                diagnostics: Vec::new(),
            };
            rightkit_onnx::init_environment_with_options(
                &selection,
                &rightkit_onnx::EnvironmentInitOptions {
                    global_pool: Some(rightkit_onnx::GlobalPool {
                        intra_threads: pool.threads,
                        inter_threads: pool.inter_threads,
                        spin: pool.spin,
                    }),
                    defer_initialization: true,
                    telemetry: None,
                    ..Default::default()
                },
            )
            .map_err(|e| {
                format!("ORT environment was initialized before global-pool experiment: {e}")
            })?;
            // CPU stages share; DirectML sharing is a separate explicit arm.
            std::env::set_var("RIGHTKIT_ONNX_GLOBAL_POOL_ACTIVE_EXPERIMENT", "1");
            if pool.encoder_shared {
                std::env::set_var("RIGHTKIT_ONNX_DML_SHARED_POOL_EXPERIMENT", "1");
            }
        }
        if plan.startup_order.as_deref().is_some_and(|order| {
            !matches!(
                order,
                "startup_before_kws" | "kws_before_startup" | "rust_before_startup"
            )
        }) || (plan.startup_order.is_some() && !plan.production_environment)
        {
            return Err("invalid startup-order probe".into());
        }
        if plan.startup_order.as_deref() == Some("rust_before_startup") {
            // Eager: commit and create the C environment now (telemetry left at ort's default, as before).
            let selection = rightkit_onnx::RuntimeSelection {
                path: rightkit_onnx::configured_runtime()
                    .ok_or("ORT runtime path is not configured")?,
                source: rightkit_onnx::CandidateSource::Explicit,
                diagnostics: Vec::new(),
            };
            rightkit_onnx::init_environment_with_options(
                &selection,
                &rightkit_onnx::EnvironmentInitOptions {
                    telemetry: None,
                    ..Default::default()
                },
            )
            .map_err(|e| format!("early Rust environment: {e}"))?;
        }
        if plan.production_environment
            && plan.startup_order.as_deref() != Some("kws_before_startup")
        {
            rightkit_dictation_inference::init_windows_ort().map_err(|e| e.to_string())?;
        }
        let whisper = if plan.whisper_first {
            std::env::set_var("RIGHTKIT_ASR_BACKEND", "whisper-turbo");
            std::env::set_var("RIGHTKIT_DICTATION_LANG", "auto");
            let runtime = AsrRuntime::load(Path::new(&args[0]), AsrEp::Cpu)?;
            if !matches!(runtime, AsrRuntime::WhisperWin { .. }) {
                return Err("Whisper initial backend not loaded".into());
            }
            Some(runtime)
        } else {
            None
        };
        let _kws = if matches!(
            plan.startup_order.as_deref(),
            Some("startup_before_kws" | "kws_before_startup")
        ) {
            let model = std::env::var_os("RIGHTKIT_KWS_MODEL_DIR")
                .ok_or("RIGHTKIT_KWS_MODEL_DIR required")?;
            Some(rightkit_dictation_inference::sherpa_kws::SherpaKws::load(
                Path::new(&model),
            )?)
        } else {
            None
        };
        if plan.startup_order.as_deref() == Some("kws_before_startup") {
            rightkit_dictation_inference::init_windows_ort().map_err(|e| e.to_string())?;
        }
        drop(whisper);
        let mut audio = Vec::with_capacity(inputs.len());
        for input in &inputs {
            let path = Path::new(&input.path);
            if hash(&fs::read(path).map_err(|e| e.to_string())?) != input.sha256 {
                return Err(format!("audio hash mismatch: {}", input.id));
            }
            let pcm = wav(path)?;
            if pcm.is_empty() || (pcm.len() as f64 / 16_000.0 - input.duration_s).abs() > 0.01 {
                return Err(format!("audio duration mismatch: {}", input.id));
            }
            audio.push(pcm);
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&args[3])
            .map_err(|e| e.to_string())?;
        let mut out = BufWriter::new(file);
        emit(
            &mut out,
            &serde_json::json!({"event":"identity", "schema":"windows-decode-probe-v1",
            "manifest_sha256":hash(&manifest_bytes), "plan_sha256":hash(&plan_bytes),
            "clips":inputs.len(), "provider":plan.provider, "model":args[0], "global_pool":plan.global_pool,
            "production_environment":plan.production_environment,
            "condition_audio":plan.condition_audio,
            "startup_order":plan.startup_order,"whisper_initial_backend_loaded":plan.whisper_first}),
        )?;
        for group in &plan.groups {
            settings(group, &group.arms[0]);
            emit(
                &mut out,
                &serde_json::json!({"event":"group_start", "group":group}),
            )?;
            let load = Instant::now();
            let mut vad = if plan.vad_order.as_deref() == Some("before_asr") {
                Some(vad_smoke()?)
            } else {
                None
            };
            let mut runtime = match AsrRuntime::load(Path::new(&args[0]), provider) {
                Ok(value) => value,
                Err(error) => {
                    emit(
                        &mut out,
                        &serde_json::json!({"event":"group_error", "group":group.label, "error":error}),
                    )?;
                    continue;
                }
            };
            if plan.vad_order.as_deref() == Some("after_asr") {
                vad = Some(vad_smoke()?);
            }
            if let Some(vad) = &mut vad {
                if vad.observe_frames(&[0.0; 512]).as_deref() != Some(&[false]) {
                    return Err("VAD failed after ASR load".into());
                }
                emit(
                    &mut out,
                    &serde_json::json!({"event":"vad_verified", "order":plan.vad_order,
                    "silence_frames":11,"ready_after_asr":true}),
                )?;
            }
            let load_ms = load.elapsed().as_secs_f64() * 1000.0;
            for arm in &group.arms {
                settings(group, arm);
                eprintln!(
                    "RIGHTKIT_DECODE_PROBE_BEGIN group={} arm={} id=__warmup__",
                    group.label, arm.label
                );
                let conditioned = plan.condition_audio.then(|| {
                    rightkit_dictation_core::audio_conditioning::condition_for_asr(
                        &audio[0], 16_000, "default",
                    )
                });
                if let Err(error) = runtime.transcribe_final_result_under_lease(
                    conditioned.as_deref().unwrap_or(&audio[0]),
                ) {
                    return Err(format!("warmup {}: {error}", arm.label));
                }
            }
            for (index, (input, pcm)) in inputs.iter().zip(audio.iter()).enumerate() {
                for offset in 0..group.arms.len() {
                    let arm = &group.arms[(index + offset) % group.arms.len()];
                    settings(group, arm);
                    eprintln!(
                        "RIGHTKIT_DECODE_PROBE_BEGIN group={} arm={} id={}",
                        group.label, arm.label, input.id
                    );
                    let cpu_before = process_cpu_ms()?;
                    let start = Instant::now();
                    let conditioned = plan.condition_audio.then(|| {
                        rightkit_dictation_core::audio_conditioning::condition_for_asr(
                            pcm, 16_000, "default",
                        )
                    });
                    let conditioning_ms = start.elapsed().as_secs_f64() * 1000.0;
                    let result = runtime
                        .transcribe_final_result_under_lease(conditioned.as_deref().unwrap_or(pcm));
                    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                    let cpu_after = process_cpu_ms()?;
                    let (text, tokens, error) = match result {
                        Ok(value) => (
                            Some(value.text),
                            value
                                .tokens
                                .into_iter()
                                .map(|v| Token {
                                    text: v.text,
                                    start: v.start,
                                    end: v.end,
                                })
                                .collect(),
                            None,
                        ),
                        Err(value) => (None, Vec::new(), Some(value)),
                    };
                    let control_intent = text
                        .as_deref()
                        .and_then(rightkit_text_polish::text_pipeline::parse_control_command)
                        .map(|command| format!("{:?}", command.intent).to_ascii_lowercase());
                    emit(
                        &mut out,
                        &Row {
                            event: "decode",
                            group: &group.label,
                            arm: &arm.label,
                            id: &input.id,
                            manifest_index: index,
                            order_index: offset,
                            audio_seconds: pcm.len() as f64 / 16_000.0,
                            elapsed_ms,
                            conditioning_ms,
                            cpu_user_ms: cpu_after.0 - cpu_before.0,
                            cpu_kernel_ms: cpu_after.1 - cpu_before.1,
                            text,
                            tokens,
                            control_intent,
                            error,
                        },
                    )?;
                }
                eprintln!(
                    "RIGHTKIT_DECODE_PROBE_PROGRESS group={} completed={}/{}",
                    group.label,
                    index + 1,
                    inputs.len()
                );
            }
            drop(runtime);
            emit(
                &mut out,
                &serde_json::json!({"event":"group_complete", "group":group.label,"load_ms":load_ms}),
            )?;
        }
        emit(&mut out, &serde_json::json!({"event":"complete"}))?;
        Ok(())
    }
    pub fn main() {
        if let Err(error) = run() {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}

#[cfg(target_os = "windows")]
fn main() {
    windows::main();
}
