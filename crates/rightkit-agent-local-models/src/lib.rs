//! Local model cookbook primitives: hardware snapshot, runtime detection, and
//! fit scoring for local OpenAI-compatible endpoints.
//!
//! Gap: this workspace contains no on-device model weights or inference engine.
//! The bounded seam below therefore reports [`LocalInferenceError::NoLocalModelAvailable`]
//! until a real backend is supplied by a product-owned integration.
//!
//! Hardware fit and runtime detection come from `rightkit-models`. See README.md.

#![deny(unsafe_code)]

pub use rightkit_models::FitVerdict;
use rightkit_models::{
    detect_inference_runtimes, hardware::total_memory_bytes, FitConfidence, Gpu, Hardware,
    InferenceRuntime, ProbeFuture, ProbeRunner, TieredRequirements,
};
use rightkit_sandbox::execution::{CommandLaunchSpec, ProcessLaunchAdapter, ProcessLaunchContext};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Hard limits applied to a local inference request before a backend is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalInferenceLimits {
    /// Maximum number of input context tokens accepted by the seam.
    pub max_context_tokens: usize,
    /// Maximum number of output tokens a backend may be asked to produce.
    pub max_output_tokens: u32,
    /// Maximum wall-clock budget a backend may receive for one request.
    pub max_wall_clock: Duration,
}

impl Default for LocalInferenceLimits {
    fn default() -> Self {
        Self {
            max_context_tokens: 8_192,
            max_output_tokens: 1_024,
            max_wall_clock: Duration::from_secs(30),
        }
    }
}

/// The minimum input needed by the bounded local inference seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalInferenceRequest {
    /// Text supplied to the local model.
    pub prompt: String,
    /// Caller-estimated token count for `prompt` and any retained context.
    pub context_tokens: usize,
    /// Requested output token budget.
    pub max_output_tokens: u32,
    /// Requested wall-clock budget for the backend operation.
    pub wall_clock: Duration,
}

/// A completed local inference result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalInferenceResponse {
    /// Text produced by the backend.
    pub text: String,
    /// Number of output tokens reported by the backend, when known.
    pub output_tokens: Option<u32>,
}

/// Typed failures from the bounded local inference seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalInferenceError {
    /// No real model backend or weights are available in this workspace.
    NoLocalModelAvailable,
    /// The request's context exceeds the configured bound.
    ContextLengthExceeded { requested: usize, maximum: usize },
    /// The request's output budget exceeds the configured bound.
    TokenBudgetExceeded { requested: u32, maximum: u32 },
    /// The request's wall-clock budget exceeds the configured bound.
    WallClockExceeded {
        requested: Duration,
        maximum: Duration,
    },
    /// The supplied backend rejected an otherwise bounded request.
    Backend(String),
}

impl std::fmt::Display for LocalInferenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoLocalModelAvailable => formatter.write_str("no local model available"),
            Self::ContextLengthExceeded { requested, maximum } => write!(
                formatter,
                "local inference context length {requested} exceeds limit {maximum}"
            ),
            Self::TokenBudgetExceeded { requested, maximum } => write!(
                formatter,
                "local inference token budget {requested} exceeds limit {maximum}"
            ),
            Self::WallClockExceeded { requested, maximum } => write!(
                formatter,
                "local inference wall clock {requested:?} exceeds limit {maximum:?}"
            ),
            Self::Backend(message) => write!(formatter, "local inference backend: {message}"),
        }
    }
}

impl std::error::Error for LocalInferenceError {}

/// A real local inference implementation supplied by a product integration.
///
/// The seam passes only a pre-validated request to this trait, so a backend
/// cannot accidentally receive work beyond the configured context, token, or
/// wall-clock limits.
pub trait LocalInferenceBackend {
    /// Execute one bounded request.
    fn infer(
        &self,
        request: &LocalInferenceRequest,
    ) -> Result<LocalInferenceResponse, LocalInferenceError>;
}

/// Validate a request without invoking a model or backend.
pub fn validate_inference_request(
    request: &LocalInferenceRequest,
    limits: LocalInferenceLimits,
) -> Result<(), LocalInferenceError> {
    if request.context_tokens > limits.max_context_tokens {
        return Err(LocalInferenceError::ContextLengthExceeded {
            requested: request.context_tokens,
            maximum: limits.max_context_tokens,
        });
    }
    if request.max_output_tokens > limits.max_output_tokens {
        return Err(LocalInferenceError::TokenBudgetExceeded {
            requested: request.max_output_tokens,
            maximum: limits.max_output_tokens,
        });
    }
    if request.wall_clock > limits.max_wall_clock {
        return Err(LocalInferenceError::WallClockExceeded {
            requested: request.wall_clock,
            maximum: limits.max_wall_clock,
        });
    }
    Ok(())
}

/// Run a request through a backend only after all three bounds pass.
pub fn infer_with_backend<B: LocalInferenceBackend>(
    backend: &B,
    request: &LocalInferenceRequest,
    limits: LocalInferenceLimits,
) -> Result<LocalInferenceResponse, LocalInferenceError> {
    validate_inference_request(request, limits)?;
    backend.infer(request)
}

/// Report the explicit missing-model condition for the current workspace.
pub fn infer_local_model(
    request: &LocalInferenceRequest,
    limits: LocalInferenceLimits,
) -> Result<LocalInferenceResponse, LocalInferenceError> {
    validate_inference_request(request, limits)?;
    Err(LocalInferenceError::NoLocalModelAvailable)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HardwareReport {
    pub cpu_threads: usize,
    pub total_memory_mb: Option<u64>,
    #[serde(default)]
    pub gpu_devices: Vec<LocalGpuInfo>,
    #[serde(default)]
    pub gpu_notes: Vec<String>,
    #[serde(default)]
    pub runtimes: Vec<LocalRuntimeStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LocalGpuInfo {
    pub name: String,
    pub vendor: Option<String>,
    pub memory_mb: Option<u64>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LocalRuntimeStatus {
    pub id: String,
    pub name: String,
    pub detected: bool,
    pub version: Option<String>,
    pub install_hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LocalModelCatalog {
    pub hardware: HardwareReport,
    #[serde(default)]
    pub recommendations: Vec<LocalModelRecommendation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LocalModelRecommendation {
    pub id: String,
    pub name: String,
    pub provider_runtime: String,
    pub model_slug: String,
    pub min_memory_mb: u64,
    pub recommended_memory_mb: u64,
    pub min_vram_mb: Option<u64>,
    pub recommended_vram_mb: Option<u64>,
    pub detected_vram_mb: Option<u64>,
    pub fit_confidence: String,
    pub quantization_notes: String,
    pub tool_call_reliability: String,
    pub fit: FitVerdict,
    pub fit_reason: String,
    pub endpoint_hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRouteSuggestion {
    pub provider_id: String,
    pub display_name: String,
    pub base_url: String,
    pub model: String,
    pub api_key_required: bool,
    pub note: String,
}

/// Probe hardware through the caller-owned confined process adapter.
///
/// The probing, parsing and de-duplication are `rightkit-models`
/// ([`Hardware::probe_with_runner`], [`detect_inference_runtimes`]); every
/// child they ask for is launched through `adapter`, so daemon callers never
/// start nvidia-smi, wmic, system_profiler or a runtime `--version` directly.
pub async fn probe_hardware_with_adapter(
    adapter: &dyn ProcessLaunchAdapter,
    context: &ProcessLaunchContext,
) -> HardwareReport {
    let runner = AdapterProbeRunner { adapter, context };
    let (hardware, runtimes) = tokio::join!(
        Hardware::probe_with_runner(&runner),
        detect_inference_runtimes(&runner),
    );
    let gpu_devices: Vec<LocalGpuInfo> = hardware.gpus.iter().map(local_gpu_info).collect();
    HardwareReport {
        cpu_threads: std::thread::available_parallelism()
            .map(|threads| threads.get())
            .unwrap_or(1),
        total_memory_mb: total_memory_bytes().map(|bytes| bytes / (1024 * 1024)),
        gpu_notes: gpu_notes(&gpu_devices),
        gpu_devices,
        runtimes: local_runtime_statuses(runtimes),
    }
}

/// the host app's wiring of RightKit's [`ProbeRunner`] onto the sandboxed
/// process adapter. Each probe is bounded by [`PROBE_IO_BUDGET`].
struct AdapterProbeRunner<'a> {
    adapter: &'a dyn ProcessLaunchAdapter,
    context: &'a ProcessLaunchContext,
}

impl ProbeRunner for AdapterProbeRunner<'_> {
    fn run<'a>(&'a self, program: &'a str, args: &'a [&'a str]) -> ProbeFuture<'a> {
        Box::pin(async move {
            probe_text_with_adapter(
                self.adapter,
                self.context,
                program,
                args.iter().map(|arg| (*arg).to_string()).collect(),
            )
            .await
        })
    }
}

/// Thin adapter from RightKit's `Gpu` to the host app's wire model; `source` is inferred from the
/// GPU backend (RightKit does not report the probe that produced the figure).
fn local_gpu_info(gpu: &Gpu) -> LocalGpuInfo {
    let cuda = gpu.backends.iter().any(|backend| backend == "cuda");
    let metal = gpu.backends.iter().any(|backend| backend == "metal");
    LocalGpuInfo {
        name: gpu.name.clone(),
        vendor: gpu_vendor(&gpu.name).or_else(|| cuda.then(|| "NVIDIA".to_string())),
        memory_mb: gpu.vram_gb.map(|gb| (gb * 1024.0).round() as u64),
        source: if cuda {
            "nvidia-smi"
        } else if metal {
            "system_profiler SPDisplaysDataType"
        } else {
            "wmic Win32_VideoController"
        }
        .to_string(),
    }
}

fn local_runtime_statuses(runtimes: Vec<InferenceRuntime>) -> Vec<LocalRuntimeStatus> {
    let mut statuses: Vec<LocalRuntimeStatus> = runtimes
        .into_iter()
        .map(|runtime| LocalRuntimeStatus {
            id: runtime.id,
            name: runtime.name,
            detected: runtime.detected,
            version: runtime.version,
            install_hint: runtime.install_hint,
        })
        .collect();
    // LM Studio has no `--version` probe (RightKit omits it too); the catalog
    // lists it so the UI can point at its local server.
    statuses.push(LocalRuntimeStatus {
        id: "lm-studio".into(),
        name: "LM Studio".into(),
        detected: false,
        version: None,
        install_hint: "Start LM Studio's local server and probe http://127.0.0.1:1234/v1/models."
            .into(),
    });
    statuses
}

/// Wall-clock bound for one discovery probe's read+wait phase. Machine
/// probes (wmic, nvidia-smi, runtime --version) are best-effort; a hung or
/// wedged child must not stall the daemon's hardware/catalog responses.
const PROBE_IO_BUDGET: Duration = Duration::from_millis(1200);

async fn probe_text_with_adapter(
    adapter: &dyn ProcessLaunchAdapter,
    context: &ProcessLaunchContext,
    program: &str,
    argv: Vec<String>,
) -> Option<(String, i32)> {
    let spec = CommandLaunchSpec {
        program: program.to_string(),
        argv,
        cwd: context.workspace_root.clone(),
        context: context.clone(),
        env: Vec::new(),
    };
    let mut child = adapter.spawn(spec).await.ok()?;
    let mut stderr = child.take_stderr();
    if let Some(mut stderr) = stderr.take() {
        tokio::spawn(async move {
            let mut buffer = [0u8; 8192];
            loop {
                match tokio::io::AsyncReadExt::read(&mut stderr, &mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
    }
    let mut stdout = match child.take_stdout() {
        Some(stdout) => stdout,
        None => {
            let _ = child.cancel().await;
            return None;
        }
    };
    let read = async {
        let mut bytes = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stdout, &mut bytes)
            .await
            .ok()?;
        Some(bytes)
    };
    let bytes = match tokio::time::timeout(PROBE_IO_BUDGET, read).await {
        Ok(Some(bytes)) => bytes,
        _ => {
            let _ = child.cancel().await;
            return None;
        }
    };
    match tokio::time::timeout(PROBE_IO_BUDGET, child.wait()).await {
        Ok(Ok(status)) => Some((String::from_utf8_lossy(&bytes).trim().to_string(), status)),
        _ => {
            let _ = child.cancel().await;
            None
        }
    }
}

pub fn model_catalog(hardware: HardwareReport) -> LocalModelCatalog {
    let candidates = [
        (
            "qwen2.5-coder-7b",
            "Qwen2.5 Coder 7B Instruct",
            "ollama",
            "qwen2.5-coder:7b",
            8192,
            16384,
            Some(6144),
            Some(8192),
            "http://127.0.0.1:11434/v1",
        ),
        (
            "qwen2.5-coder-14b",
            "Qwen2.5 Coder 14B Instruct",
            "ollama",
            "qwen2.5-coder:14b",
            16384,
            32768,
            Some(10240),
            Some(16384),
            "http://127.0.0.1:11434/v1",
        ),
        (
            "deepseek-coder-v2-lite",
            "DeepSeek Coder V2 Lite",
            "lm-studio",
            "deepseek-coder-v2-lite-instruct",
            12288,
            24576,
            Some(8192),
            Some(12288),
            "http://127.0.0.1:1234/v1",
        ),
        (
            "llama-cpp-coder-7b",
            "Open coding 7B GGUF via llama.cpp",
            "llama.cpp",
            "local-coder-7b",
            8192,
            16384,
            Some(4096),
            Some(8192),
            "http://127.0.0.1:8080/v1",
        ),
    ];
    let detected_vram_mb = hardware
        .gpu_devices
        .iter()
        .filter_map(|gpu| gpu.memory_mb)
        .max();

    let recommendations = candidates
        .into_iter()
        .map(
            |(
                id,
                name,
                runtime,
                slug,
                min_memory_mb,
                recommended_memory_mb,
                min_vram_mb,
                recommended_vram_mb,
                endpoint,
            )| {
                let (fit, fit_reason, fit_confidence) = fit_for_resources(
                    hardware.total_memory_mb,
                    detected_vram_mb,
                    min_memory_mb,
                    recommended_memory_mb,
                    min_vram_mb,
                    recommended_vram_mb,
                );
                LocalModelRecommendation {
                    id: id.to_string(),
                    name: name.to_string(),
                    provider_runtime: runtime.to_string(),
                    model_slug: slug.to_string(),
                    min_memory_mb,
                    recommended_memory_mb,
                    min_vram_mb,
                    recommended_vram_mb,
                    detected_vram_mb,
                    fit_confidence,
                    quantization_notes: quantization_notes(min_vram_mb, recommended_vram_mb),
                    tool_call_reliability: tool_call_reliability(runtime, slug),
                    fit,
                    fit_reason,
                    endpoint_hint: endpoint.to_string(),
                }
            },
        )
        .collect();

    LocalModelCatalog {
        hardware,
        recommendations,
    }
}

pub fn provider_route_suggestion(
    provider_id: impl Into<String>,
    base_url: impl Into<String>,
    model: impl Into<String>,
) -> ProviderRouteSuggestion {
    let provider_id = provider_id.into();
    let base_url = base_url.into();
    let model = model.into();
    ProviderRouteSuggestion {
        display_name: format!("Local: {model}"),
        provider_id,
        base_url,
        model,
        api_key_required: false,
        note: "Use this as an OpenAI-compatible provider route after confirming /v1/models health."
            .to_string(),
    }
}

/// Tiered RAM+VRAM verdict, computed by `rightkit_models::Hardware::tiered_fit` (the MB figures
/// are converted to the GB it takes).
fn fit_for_resources(
    total_memory_mb: Option<u64>,
    detected_vram_mb: Option<u64>,
    min_memory_mb: u64,
    recommended_memory_mb: u64,
    min_vram_mb: Option<u64>,
    recommended_vram_mb: Option<u64>,
) -> (FitVerdict, String, String) {
    let gb = |mb: u64| mb as f64 / 1024.0;
    let host = Hardware {
        memory_gb: total_memory_mb.map(gb),
        gpus: detected_vram_mb
            .map(|mb| Gpu {
                name: String::new(),
                vram_gb: Some(gb(mb)),
                unified: false,
                // The raw MiB keeps exact fit checks; no probe named it.
                vram_mib: Some(mb),
                source: None,
                backends: Vec::new(),
            })
            .into_iter()
            .collect(),
        ..Hardware::default()
    };
    let fit = host.tiered_fit(&TieredRequirements {
        min_memory_gb: gb(min_memory_mb),
        recommended_memory_gb: gb(recommended_memory_mb),
        min_vram_gb: min_vram_mb.map(gb),
        recommended_vram_gb: recommended_vram_mb.map(gb),
    });
    let confidence = match fit.confidence {
        FitConfidence::High => "high",
        FitConfidence::Medium => "medium",
        FitConfidence::Low => "low",
    };
    (fit.verdict, fit.reason, confidence.to_string())
}

fn quantization_notes(min_vram_mb: Option<u64>, recommended_vram_mb: Option<u64>) -> String {
    match (min_vram_mb, recommended_vram_mb) {
        (Some(minimum), Some(recommended)) => format!(
            "Plan for quantized local weights: about {minimum} MB VRAM minimum, {recommended} MB preferred for smoother GPU offload."
        ),
        _ => "Quantization target depends on the server/model file; verify the runtime's memory estimate before loading.".to_string(),
    }
}

fn tool_call_reliability(runtime: &str, slug: &str) -> String {
    format!(
        "{runtime} route for {slug}: use an OpenAI-compatible server with tool-call/JSON support enabled; smaller local models may need retries or stricter schemas."
    )
}

fn gpu_notes(devices: &[LocalGpuInfo]) -> Vec<String> {
    if devices.is_empty() {
        return vec![
            "No GPU/VRAM details were detected. Local models can still run on CPU, but GPU offload should be verified manually.".to_string(),
        ];
    }
    devices
        .iter()
        .map(|gpu| match gpu.memory_mb {
            Some(memory) => format!(
                "{} reports {} MB VRAM via {}.",
                gpu.name, memory, gpu.source
            ),
            None => format!(
                "{} was detected via {}, but VRAM was not reported.",
                gpu.name, gpu.source
            ),
        })
        .collect()
}

fn gpu_vendor(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if lower.contains("nvidia") || lower.contains("geforce") || lower.contains("rtx") {
        Some("NVIDIA".to_string())
    } else if lower.contains("amd") || lower.contains("radeon") {
        Some("AMD".to_string())
    } else if lower.contains("intel") || lower.contains("arc") {
        Some("Intel".to_string())
    } else if lower.contains("apple") {
        Some("Apple".to_string())
    } else {
        None
    }
}
