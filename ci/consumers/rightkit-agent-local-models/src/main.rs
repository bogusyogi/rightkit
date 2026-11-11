// rightkit-agent-local-models 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Builds the local model catalog over stated hardware reports, a provider route suggestion, and runs the
// bounded inference seam. `probe_hardware_with_adapter` launches probe processes and is never called.
use std::cell::Cell;
use std::time::Duration;

use rightkit_agent_local_models::{
    infer_local_model, infer_with_backend, model_catalog, provider_route_suggestion,
    validate_inference_request, FitVerdict, HardwareReport, LocalGpuInfo, LocalInferenceBackend,
    LocalInferenceError, LocalInferenceLimits, LocalInferenceRequest, LocalInferenceResponse,
};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn report(total_memory_mb: Option<u64>, vram_mb: Option<u64>) -> HardwareReport {
    HardwareReport {
        cpu_threads: 8,
        total_memory_mb,
        gpu_devices: vram_mb
            .map(|mb| LocalGpuInfo {
                name: "consumer-gpu".into(),
                vendor: None,
                memory_mb: Some(mb),
                source: "stated".into(),
            })
            .into_iter()
            .collect(),
        gpu_notes: Vec::new(),
        runtimes: Vec::new(),
    }
}

struct EchoBackend {
    calls: Cell<u32>,
}

impl LocalInferenceBackend for EchoBackend {
    fn infer(
        &self,
        request: &LocalInferenceRequest,
    ) -> Result<LocalInferenceResponse, LocalInferenceError> {
        self.calls.set(self.calls.get() + 1);
        Ok(LocalInferenceResponse {
            text: request.prompt.clone(),
            output_tokens: Some(1),
        })
    }
}

fn request(
    context_tokens: usize,
    max_output_tokens: u32,
    wall_clock: Duration,
) -> LocalInferenceRequest {
    LocalInferenceRequest {
        prompt: "hello".into(),
        context_tokens,
        max_output_tokens,
        wall_clock,
    }
}

fn main() {
    // Catalog fit over stated hardware.
    let big = model_catalog(report(Some(65_536), Some(24_576)));
    check("catalog lists candidates", !big.recommendations.is_empty());
    check(
        "large host fits every candidate",
        big.recommendations
            .iter()
            .all(|r| r.fit == FitVerdict::Good),
    );
    check(
        "detected vram carried",
        big.recommendations
            .iter()
            .all(|r| r.detected_vram_mb == Some(24_576)),
    );
    let small = model_catalog(report(Some(2_048), None));
    check(
        "small host fits nothing",
        small
            .recommendations
            .iter()
            .all(|r| r.fit == FitVerdict::TooLarge),
    );
    let unknown = model_catalog(report(None, None));
    check(
        "unknown memory is unknown",
        unknown
            .recommendations
            .iter()
            .all(|r| r.fit == FitVerdict::Unknown && r.fit_confidence == "low"),
    );
    check(
        "every endpoint is loopback",
        big.recommendations
            .iter()
            .all(|r| r.endpoint_hint.starts_with("http://127.0.0.1:")),
    );

    // Provider route suggestion.
    let route =
        provider_route_suggestion("ollama", "http://127.0.0.1:11434/v1", "qwen2.5-coder:7b");
    check(
        "route suggestion",
        route.provider_id == "ollama"
            && route.base_url == "http://127.0.0.1:11434/v1"
            && route.model == "qwen2.5-coder:7b"
            && route.display_name == "Local: qwen2.5-coder:7b"
            && !route.api_key_required,
    );

    // Bounded inference seam.
    let limits = LocalInferenceLimits::default();
    let within = request(100, 10, Duration::from_secs(1));
    check(
        "request within limits",
        validate_inference_request(&within, limits).is_ok(),
    );
    check(
        "context over limit refused",
        matches!(
            validate_inference_request(
                &request(limits.max_context_tokens + 1, 10, Duration::from_secs(1)),
                limits
            ),
            Err(LocalInferenceError::ContextLengthExceeded { .. })
        ),
    );
    check(
        "output budget over limit refused",
        matches!(
            validate_inference_request(
                &request(100, limits.max_output_tokens + 1, Duration::from_secs(1)),
                limits
            ),
            Err(LocalInferenceError::TokenBudgetExceeded { .. })
        ),
    );
    check(
        "wall clock over limit refused",
        matches!(
            validate_inference_request(
                &request(100, 10, limits.max_wall_clock + Duration::from_secs(1)),
                limits
            ),
            Err(LocalInferenceError::WallClockExceeded { .. })
        ),
    );
    check(
        "no local model without a backend",
        infer_local_model(&within, limits) == Err(LocalInferenceError::NoLocalModelAvailable),
    );
    let backend = EchoBackend {
        calls: Cell::new(0),
    };
    let over = request(limits.max_context_tokens + 1, 10, Duration::from_secs(1));
    check(
        "backend not reached over the limit",
        infer_with_backend(&backend, &over, limits).is_err() && backend.calls.get() == 0,
    );
    check(
        "backend reached within the limit",
        infer_with_backend(&backend, &within, limits).map(|r| r.text) == Ok("hello".to_string())
            && backend.calls.get() == 1,
    );
    println!("OK rightkit-agent-local-models 0.1.0 (registry consumer)");
}
