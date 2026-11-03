//! ORT session construction and execution-provider selection.

use std::path::Path;

use crate::onnx_tdt::{Error, Result};

/// Intra-op threads for the Windows decoder/joint session (capped by the
/// machine's available parallelism).
const WINDOWS_DECODER_THREADS: usize = 6;

/// Which ORT execution provider to build sessions with.
///
/// The variants are gated to the platforms that can actually supply them, so an
/// unavailable provider is a compile error rather than a runtime fallback that
/// silently changes performance characteristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionProvider {
    #[default]
    Cpu,
    #[cfg(target_os = "windows")]
    DirectMl,
    #[cfg(target_os = "macos")]
    CoreMl,
}

#[derive(Debug, Clone, Default)]
pub struct SessionOptions {
    pub provider: ExecutionProvider,
    /// DirectML adapter index. Onboarding persists a choice on multi-GPU
    /// machines; `None` lets the runtime pick.
    pub device_id: Option<i32>,
    /// Intra-op threads. Windows CPU stages share pool; encoder keeps automatic
    /// private threads. Explicit sizes opt out.
    pub threads: Option<usize>,
    /// Let the CPU intra-op pool spin between parallel sections. Only the
    /// Windows decoder/joint stage turns this on (see `cpu_only_for_stage`);
    /// every other CPU session, and the shared pool, keep spinning off.
    pub spinning: bool,
}

impl SessionOptions {
    pub fn new(provider: ExecutionProvider) -> Self {
        Self {
            provider,
            device_id: None,
            threads: None,
            spinning: false,
        }
    }

    pub fn with_device_id(mut self, device_id: Option<i32>) -> Self {
        self.device_id = device_id;
        self
    }

    /// The same options with the provider forced to CPU. Used for the mel and
    /// decoder/joint stages, which are dispatch-bound rather than compute-bound.
    pub(crate) fn cpu_only_for_stage(&self, stage: &str) -> Self {
        // Opt-in native benchmark override. Keep encoder options untouched.
        // `dev_var!` reads nothing outside test builds, so the name goes unused there.
        let read_threads = |_key: &str| {
            dev_var!(_key)
                .ok()
                .and_then(|value| parse_thread_override(&value))
        };
        let experiment_threads = read_threads(&format!("RIGHTKIT_ONNX_{stage}_THREADS_EXPERIMENT"))
            .or_else(|| {
                if matches!(stage, "PREDICTOR" | "JOINT") {
                    read_threads("RIGHTKIT_ONNX_DECODER_THREADS_EXPERIMENT")
                } else {
                    None
                }
            })
            .or_else(|| read_threads("RIGHTKIT_ONNX_CPU_THREADS_EXPERIMENT"));
        // Windows decoder/joint: a private pool that spins, 6 intra threads.
        // Paired AB/BA on the Dell (Ultra, beam 2, RTX): spin + 8 threads beat
        // the shared non-spinning pool 6/6 (run 17.9 -> 10.4 ms per window);
        // 6 threads matched 8 (+0.6 ms, not significant) with ~30 % less spin
        // tail. Spinning costs nothing at idle (0 % CPU from 2 s after the last
        // call) and ~2 core-seconds per dictation. RIGHTKIT_ONNX_CPU_SPIN_EXPERIMENT=0
        // restores the shared non-spinning pool.
        #[cfg(target_os = "windows")]
        let decoder_spins = stage == "DECODER"
            && dev_var!("RIGHTKIT_ONNX_CPU_SPIN_EXPERIMENT").as_deref() != Ok("0");
        #[cfg(not(target_os = "windows"))]
        let decoder_spins = false;
        let threads = experiment_threads.or(self.threads).or_else(|| {
            decoder_spins.then(|| {
                std::thread::available_parallelism()
                    .map(usize::from)
                    .unwrap_or(1)
                    .min(WINDOWS_DECODER_THREADS)
            })
        });
        Self {
            provider: ExecutionProvider::Cpu,
            threads,
            spinning: decoder_spins,
            ..self.clone()
        }
    }

    pub(crate) fn build(&self, path: &Path) -> Result<rightkit_onnx::ort::session::Session> {
        self.build_inner(path, false, false)
    }

    /// Build the encoder. `static_shape` says the graph declares a fixed time
    /// dimension, which on DirectML requires the fusion workaround below.
    pub(crate) fn build_encoder(
        &self,
        path: &Path,
        static_shape: bool,
    ) -> Result<rightkit_onnx::ort::session::Session> {
        self.build_inner(path, static_shape, true)
    }

    fn build_inner(
        &self,
        path: &Path,
        static_shape: bool,
        encoder: bool,
    ) -> Result<rightkit_onnx::ort::session::Session> {
        if !path.is_file() {
            return Err(Error::MissingArtifact(path.display().to_string()));
        }
        #[cfg(target_os = "windows")]
        let shared_pool = crate::onnx_tdt::environment::windows_shared_pool_active()?;
        #[cfg(not(target_os = "windows"))]
        let shared_pool = dev_var!("RIGHTKIT_ONNX_GLOBAL_POOL_ACTIVE_EXPERIMENT")
            .as_deref()
            == Ok("1");
        let result = self.build_with_pool(path, static_shape, encoder, shared_pool);
        #[cfg(target_os = "windows")]
        if let Err(Error::Runtime(error)) = &result {
            if shared_pool && error.contains("EnvCreatedWithGlobalThreadPools() was false") {
                crate::onnx_tdt::environment::use_private_pools(error);
                return self.build_with_pool(path, static_shape, encoder, false);
            }
        }
        result
    }

    fn build_with_pool(
        &self,
        path: &Path,
        static_shape: bool,
        encoder: bool,
        shared_pool: bool,
    ) -> Result<rightkit_onnx::ort::session::Session> {
        let mut builder = rightkit_onnx::ort::session::Session::builder(
            rightkit_onnx::ort_environment()
                .map_err(|e| Error::Runtime(format!("ort environment: {e}")))?,
        )
        .map_err(|e| Error::Runtime(format!("session builder: {e}")))?;

        // DirectML keeps its existing independent pool. Sharing remains probe-only.
        if self.is_directml()
            && dev_var!("RIGHTKIT_ONNX_DML_SHARED_POOL_EXPERIMENT").as_deref()
                != Ok("1")
        {
            builder = builder
                .with_independent_thread_pool()
                .map_err(|e| Error::Runtime(format!("independent DirectML pool: {e}")))?;
        }

        // Explicit CPU thread settings require a private pool; ORT otherwise
        // ignores them when a global pool exists.
        #[cfg(target_os = "windows")]
        let private_cpu_pool = self.threads.is_some()
            || !shared_pool
            || (encoder
                && dev_var!("RIGHTKIT_ONNX_CPU_ENCODER_SHARED_EXPERIMENT")
                    .as_deref()
                    != Ok("1"));
        #[cfg(not(target_os = "windows"))]
        let private_cpu_pool = self.threads.is_some() && shared_pool;
        if matches!(self.provider, ExecutionProvider::Cpu) && private_cpu_pool {
            builder = builder
                .with_independent_thread_pool()
                .map_err(|e| Error::Runtime(format!("independent CPU stage: {e}")))?;
        }

        #[cfg(target_os = "windows")]
        if matches!(self.provider, ExecutionProvider::Cpu) {
            // Spinning only takes effect on a private pool; the shared pool is
            // created with spinning off in `environment.rs`.
            builder = builder
                .with_intra_op_spinning(self.spinning && private_cpu_pool)
                .map_err(|e| Error::Runtime(format!("CPU intra spinning: {e}")))?
                .with_inter_op_spinning(false)
                .map_err(|e| Error::Runtime(format!("CPU inter spinning: {e}")))?;
            if self.spinning {
                tracing::info!(
                    model = %path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                    intra_threads = self.threads,
                    private_pool = private_cpu_pool,
                    spinning = private_cpu_pool,
                    "Windows CPU decoder session"
                );
            }
        }

        // DirectML graph-fusion bug (onnxruntime-directml 1.24.4): the fused
        // kernel for a STATIC-shape conformer encoder silently produces
        // near-zero output on DML — CPU and dynamic-shape are both correct — so
        // the TDT joint emits blank at every frame and the transcript comes back
        // EMPTY, with no error and nothing in the log. Confirmed
        // precision-independent (fp32/fp16/4-bit all collapse) and triggered by
        // shape, via a same-bytes static pin of a proven dynamic model.
        //
        // Disabling only the DML fusion pass restores CPU-parity output and
        // keeps every other optimization. Level1 is belt-and-suspenders: it
        // prevents the CPU-side Level3 fusions (QuickGelu and friends) that feed
        // the DML partition compiler, which left a residual collapse when
        // `disable_graph_fusion` was set alone.
        //
        // Costs roughly 2x encoder time, which is why it is scoped to exactly
        // static + DirectML. REMOVAL CONDITION: an onnxruntime-directml release
        // that fixes static-conformer fusion — drop it once a static DML decode
        // passes the corpus without it.
        if static_shape && self.is_directml() && !allow_dml_fusion() {
            match DmlOptimizerBisect::from_env()? {
                // Shipping guard: Level1 + DML graph fusion off.
                None => {
                    builder = builder
                        .with_config_entry("ep.dml.disable_graph_fusion", "1")
                        .map_err(|e| Error::Runtime(format!("disable_graph_fusion: {e}")))?
                        .with_optimization_level(
                            rightkit_onnx::ort::session::builder::GraphOptimizationLevel::Level1,
                        )
                        .map_err(|e| Error::Runtime(format!("optimization level: {e}")))?;
                }
                Some(bisect) => builder = bisect.apply(builder)?,
            }
        }

        // Dev-only DirectML diagnostics (ORT-01). Neither changes numerics.
        if self.is_directml() {
            // Per-node placement: ORT prints "Node placements" / "All nodes
            // placed on [...]" at VERBOSE while the session is created. The
            // engine's ORT environment has no custom logger, so it goes to stderr.
            if dev_var!("RIGHTKIT_ONNX_DML_LOG_PLACEMENT").as_deref() == Ok("1") {
                builder = builder
                    .with_log_level(rightkit_onnx::ort::logging::LogLevel::Verbose)
                    .map_err(|e| Error::Runtime(format!("DirectML log level: {e}")))?;
            }
            // Per-op profile JSON, mirroring RIGHTKIT_ONNX_CPU_PROFILE_EXPERIMENT for
            // DirectML sessions. ORT writes it when the session is dropped.
            if let Ok(dir) = dev_var!("RIGHTKIT_ONNX_DML_PROFILE") {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "stage".into());
                let prefix = std::path::Path::new(&dir).join(format!("ort-profile-dml-{stem}"));
                tracing::warn!(prefix = %prefix.display(), "DirectML session profiling enabled");
                builder = builder
                    .with_profiling(prefix)
                    .map_err(|e| Error::Runtime(format!("DirectML profiling: {e}")))?;
            }
        }

        if let Some(threads) = self.threads {
            builder = builder
                .with_intra_threads(threads)
                .map_err(|e| Error::Runtime(format!("intra threads: {e}")))?;
        }
        #[cfg(target_os = "windows")]
        if !shared_pool
            && !encoder
            && self.threads.is_none()
            && dev_var!("RIGHTKIT_ONNX_LEGACY_ENV_EXPERIMENT").as_deref() != Ok("1")
            && matches!(self.provider, ExecutionProvider::Cpu)
        {
            builder = builder
                .with_intra_threads(crate::onnx_tdt::environment::windows_cpu_threads())
                .map_err(|e| Error::Runtime(format!("fallback CPU threads: {e}")))?;
        }

        // These opt-in controls override CPU stages only. Preserve DirectML's
        // static-encoder fusion guard.
        if matches!(self.provider, ExecutionProvider::Cpu) {
            if let Ok(value) = dev_var!("RIGHTKIT_ONNX_CPU_PARALLEL_EXPERIMENT") {
                let enabled = match value.as_str() {
                    "0" => false,
                    "1" => true,
                    other => {
                        return Err(Error::Runtime(format!(
                            "invalid CPU parallel experiment: {other}"
                        )))
                    }
                };
                builder = builder
                    .with_parallel_execution(enabled)
                    .map_err(|e| Error::Runtime(format!("CPU parallel execution: {e}")))?;
            }
            if let Ok(value) = dev_var!("RIGHTKIT_ONNX_CPU_INTER_THREADS_EXPERIMENT")
            {
                let threads = value
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=64).contains(n))
                    .ok_or_else(|| Error::Runtime(format!("invalid CPU inter threads: {value}")))?;
                builder = builder
                    .with_inter_threads(threads)
                    .map_err(|e| Error::Runtime(format!("CPU inter threads: {e}")))?;
            }
            if let Ok(value) = dev_var!("RIGHTKIT_ONNX_CPU_SPIN_EXPERIMENT") {
                let enabled = match value.as_str() {
                    "0" => false,
                    "1" => true,
                    other => {
                        return Err(Error::Runtime(format!(
                            "invalid CPU spin experiment: {other}"
                        )))
                    }
                };
                builder = builder
                    .with_intra_op_spinning(enabled)
                    .map_err(|e| Error::Runtime(format!("CPU spinning: {e}")))?;
            }
            // Dev-only: write ORT's per-op profile (JSON) for this CPU stage into the given directory.
            if let Ok(dir) = dev_var!("RIGHTKIT_ONNX_CPU_PROFILE_EXPERIMENT") {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "stage".into());
                let prefix = std::path::Path::new(&dir).join(format!("ort-profile-{stem}"));
                builder = builder
                    .with_profiling(prefix)
                    .map_err(|e| Error::Runtime(format!("CPU profiling: {e}")))?;
            }
            // Dev-only: raw ORT session config entries for CPU stages, "key=value|key=value".
            if let Ok(entries) = dev_var!("RIGHTKIT_ONNX_CPU_CONFIG_EXPERIMENT") {
                for entry in entries.split('|').filter(|e| !e.trim().is_empty()) {
                    let (key, value) = entry.split_once('=').ok_or_else(|| {
                        Error::Runtime(format!("invalid CPU config experiment entry: {entry}"))
                    })?;
                    builder = builder
                        .with_config_entry(key.trim(), value.trim())
                        .map_err(|e| Error::Runtime(format!("CPU config entry {key}: {e}")))?;
                }
            }
            if let Ok(value) = dev_var!("RIGHTKIT_ONNX_CPU_OPT_EXPERIMENT") {
                use rightkit_onnx::ort::session::builder::GraphOptimizationLevel;
                let level = match value.as_str() {
                    "disabled" => GraphOptimizationLevel::Disable,
                    "level1" => GraphOptimizationLevel::Level1,
                    "level2" => GraphOptimizationLevel::Level2,
                    "level3" => GraphOptimizationLevel::Level3,
                    other => {
                        return Err(Error::Runtime(format!(
                            "invalid CPU optimization experiment: {other}"
                        )))
                    }
                };
                builder = builder
                    .with_optimization_level(level)
                    .map_err(|e| Error::Runtime(format!("CPU optimization: {e}")))?;
            }
            if let Ok(value) = dev_var!("RIGHTKIT_ONNX_CPU_MEMORY_PATTERN_EXPERIMENT")
            {
                let enabled = match value.as_str() {
                    "0" => false,
                    "1" => true,
                    other => {
                        return Err(Error::Runtime(format!(
                            "invalid CPU memory pattern experiment: {other}"
                        )))
                    }
                };
                builder = builder
                    .with_memory_pattern(enabled)
                    .map_err(|e| Error::Runtime(format!("CPU memory pattern: {e}")))?;
            }
        }

        builder = self.apply_provider(builder)?;

        let session = builder
            .commit_from_file(path)
            .map_err(|e| Error::Runtime(format!("load {}: {e}", path.display())))?;
        self.log_execution_provider(encoder);
        Ok(session)
    }

    /// One structured line per session load. DirectML is registered with
    /// `error_on_failure()`, so reaching this point means DirectML registered.
    /// ORT may still place individual nodes on CPU; that is not observable here.
    /// CoreML is registered without `error_on_failure()`, so its result is unknown.
    fn log_execution_provider(&self, encoder: bool) {
        let requested = match self.provider {
            ExecutionProvider::Cpu => "cpu",
            #[cfg(target_os = "windows")]
            ExecutionProvider::DirectMl => "directml",
            #[cfg(target_os = "macos")]
            ExecutionProvider::CoreMl => "coreml",
        };
        let registered = match self.provider {
            ExecutionProvider::Cpu => "cpu",
            #[cfg(target_os = "windows")]
            ExecutionProvider::DirectMl => "directml",
            #[cfg(target_os = "macos")]
            ExecutionProvider::CoreMl => "unknown",
        };
        tracing::info!(
            event = "asr_execution_provider",
            requested,
            registered,
            encoder,
            "ASR session execution provider"
        );
    }

    fn is_directml(&self) -> bool {
        #[cfg(target_os = "windows")]
        {
            matches!(self.provider, ExecutionProvider::DirectMl)
        }
        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    }

    #[allow(unused_mut, unused_variables)]
    fn apply_provider(
        &self,
        mut builder: rightkit_onnx::ort::session::builder::SessionBuilder,
    ) -> Result<rightkit_onnx::ort::session::builder::SessionBuilder> {
        match self.provider {
            ExecutionProvider::Cpu => {
                if let Ok(value) = dev_var!("RIGHTKIT_ONNX_CPU_ARENA_EXPERIMENT") {
                    let enabled = match value.as_str() {
                        "0" => false,
                        "1" => true,
                        other => {
                            return Err(Error::Runtime(format!(
                                "invalid CPU arena experiment: {other}"
                            )))
                        }
                    };
                    builder = builder
                        .with_execution_providers([rightkit_onnx::ort::ep::CPU::default()
                            .with_arena_allocator(enabled)
                            .build()])
                        .map_err(|e| Error::Runtime(format!("CPU arena: {e}")))?;
                }
            }
            #[cfg(target_os = "windows")]
            ExecutionProvider::DirectMl => {
                let mut ep = rightkit_onnx::ort::ep::DirectML::default();
                // ORT-19: adapter preference applies only when no adapter is
                // pinned (ORT ignores it with a device id). Default = today.
                let preference = dml_adapter_preference()?;
                if let Some(device_id) = self.device_id {
                    ep = ep.with_device_id(device_id);
                    if preference.is_some() {
                        tracing::warn!(
                            device_id,
                            "RIGHTKIT_ONNX_DML_ADAPTER_PREFERENCE ignored: a DirectML adapter is pinned"
                        );
                    }
                } else if let Some(preference) = preference {
                    tracing::warn!(?preference, "DirectML adapter preference override");
                    ep = ep.with_performance_preference(preference);
                }
                builder = builder
                    .with_execution_providers([ep.build().error_on_failure()])
                    .map_err(|e| Error::Runtime(format!("directml: {e}")))?;
            }
            #[cfg(target_os = "macos")]
            ExecutionProvider::CoreMl => {
                builder = builder
                    .with_execution_providers([rightkit_onnx::ort::ep::CoreML::default().build()])
                    .map_err(|e| Error::Runtime(format!("coreml: {e}")))?;
            }
        }
        Ok(builder)
    }
}

/// A `RIGHTKIT_ONNX_*_THREADS_EXPERIMENT` value: an explicit count in 1..=64, or
/// `physical` for the audit's proposed decoder default (ORT-05): physical
/// cores, capped like today's logical-CPU default at
/// `WINDOWS_DECODER_THREADS`. Anything else is ignored, as before.
fn parse_thread_override(value: &str) -> Option<usize> {
    match value.trim() {
        "physical" => Some(physical_cores().clamp(1, WINDOWS_DECODER_THREADS)),
        other => other
            .parse::<usize>()
            .ok()
            .filter(|threads| (1..=64).contains(threads)),
    }
}

/// Physical cores on Windows; available parallelism elsewhere (no portable
/// topology query, and the non-Windows path is a test host only).
fn physical_cores() -> usize {
    #[cfg(target_os = "windows")]
    if let Some(cores) = crate::onnx_tdt::environment::physical_core_count() {
        return cores;
    }
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
}

/// `RIGHTKIT_ONNX_DML_ADAPTER_PREFERENCE=default|high_performance|minimum_power`
/// (ORT-19). Unset or `default` keeps today's registration untouched.
#[cfg(target_os = "windows")]
fn dml_adapter_preference(
) -> Result<Option<rightkit_onnx::ort::ep::directml::PerformancePreference>> {
    use rightkit_onnx::ort::ep::directml::PerformancePreference;
    match dev_var!("RIGHTKIT_ONNX_DML_ADAPTER_PREFERENCE")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        None | Some("") | Some("default") => Ok(None),
        Some("high_performance") => Ok(Some(PerformancePreference::HighPerformance)),
        Some("minimum_power") => Ok(Some(PerformancePreference::MinimumPower)),
        Some(other) => Err(Error::Runtime(format!(
            "invalid RIGHTKIT_ONNX_DML_ADAPTER_PREFERENCE: {other}"
        ))),
    }
}

/// ORT-03: per-pass bisect of the DirectML static-encoder optimizer bug.
/// Replaces the blanket Level1 + no-fusion guard ONLY when one of its switches
/// is set; unset, the shipping guard applies unchanged.
///
/// - `RIGHTKIT_ONNX_DML_DISABLE_OPTIMIZERS=<A,B,..>|none`: names passed to
///   `optimization.disable_specified_optimizers`.
/// - `RIGHTKIT_ONNX_DML_OPT_LEVEL=disabled|level1|level2|level3` (default `level3`).
/// - `RIGHTKIT_ONNX_DML_GRAPH_FUSION=0|1` (default `1`).
///
/// The bug class is SILENT wrong output, so an arm is fit for live dictation
/// only after its encoder output correlates with the CPU encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DmlOptimizerBisect {
    disabled: Vec<String>,
    level: &'static str,
    graph_fusion: bool,
}

impl DmlOptimizerBisect {
    fn from_env() -> Result<Option<Self>> {
        Self::parse(
            dev_var!("RIGHTKIT_ONNX_DML_DISABLE_OPTIMIZERS")
                .ok()
                .as_deref(),
            dev_var!("RIGHTKIT_ONNX_DML_OPT_LEVEL")
                .ok()
                .as_deref(),
            dev_var!("RIGHTKIT_ONNX_DML_GRAPH_FUSION")
                .ok()
                .as_deref(),
        )
    }

    fn parse(
        disabled: Option<&str>,
        level: Option<&str>,
        graph_fusion: Option<&str>,
    ) -> Result<Option<Self>> {
        if disabled.is_none() && level.is_none() && graph_fusion.is_none() {
            return Ok(None);
        }
        let disabled = match disabled.map(str::trim) {
            None | Some("") | Some("none") => Vec::new(),
            Some(list) => list
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect(),
        };
        let level = match level.map(str::trim).unwrap_or("level3") {
            "disabled" => "disabled",
            "level1" => "level1",
            "level2" => "level2",
            "level3" | "all" => "level3",
            other => {
                return Err(Error::Runtime(format!(
                    "invalid RIGHTKIT_ONNX_DML_OPT_LEVEL: {other}"
                )))
            }
        };
        let graph_fusion = match graph_fusion.map(str::trim).unwrap_or("1") {
            "0" => false,
            "1" => true,
            other => {
                return Err(Error::Runtime(format!(
                    "invalid RIGHTKIT_ONNX_DML_GRAPH_FUSION: {other}"
                )))
            }
        };
        Ok(Some(Self {
            disabled,
            level,
            graph_fusion,
        }))
    }

    fn apply(
        &self,
        mut builder: rightkit_onnx::ort::session::builder::SessionBuilder,
    ) -> Result<rightkit_onnx::ort::session::builder::SessionBuilder> {
        use rightkit_onnx::ort::session::builder::GraphOptimizationLevel;
        tracing::warn!(
            event = "asr_dml_optimizer_bisect",
            level = self.level,
            graph_fusion = self.graph_fusion,
            disabled = %self.disabled.join(","),
            "DirectML static-encoder optimizer bisect: output may be WRONG; gate on CPU correlation"
        );
        if !self.graph_fusion {
            builder = builder
                .with_config_entry("ep.dml.disable_graph_fusion", "1")
                .map_err(|e| Error::Runtime(format!("disable_graph_fusion: {e}")))?;
        }
        if !self.disabled.is_empty() {
            builder = builder
                .with_config_entry(
                    "optimization.disable_specified_optimizers",
                    self.disabled.join(","),
                )
                .map_err(|e| Error::Runtime(format!("disable_specified_optimizers: {e}")))?;
        }
        let level = match self.level {
            "disabled" => GraphOptimizationLevel::Disable,
            "level1" => GraphOptimizationLevel::Level1,
            "level2" => GraphOptimizationLevel::Level2,
            _ => GraphOptimizationLevel::Level3,
        };
        builder
            .with_optimization_level(level)
            .map_err(|e| Error::Runtime(format!("optimization level: {e}")))
    }
}

/// Offline-measurement escape hatch: keeps full Level3 fusion so a static
/// encoder can be timed against a dynamic one on equal footing. NEVER set this
/// in the shipping app — the bug it sidesteps is silent, not loud.
fn allow_dml_fusion() -> bool {
    dev_var!("RIGHTKIT_ASR_ALLOW_DML_FUSION")
        .map(|v| v == "1")
        .unwrap_or(false)
}
