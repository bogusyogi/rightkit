//! Local Qwen3-TTS 0.6B voice (QTS), synchronous, no API key, no spend.
//!
//! Synthesis runs in the warm QTS worker process (Metal on macOS,
//! Vulkan on Windows) driven by `rightkit-tts` through the light `crate::qts` facade. Long text is
//! chunked; there is no frame cap beyond a per-chunk runaway guard.
//!
//! Params (all optional):
//! - `voice_prompt`: path of an x-vector voice record under
//!   `<data>/voices/local/` (made by `crate::qts::client::clone_from_wav` /
//!   `clone_into_library`). Relative paths resolve inside that folder.
//! - `language`: `en` (default), `zh`, `ja`, `ko`, `de`, `fr`, `es`, `it`, `pt`, `ru`.
//! - `temperature`, `top_p`, `top_k`, `max_chunk_chars`: sampling/chunk tuning.
//!
//! Cancellation: the job runner cannot reach into a blocking `submit`, so
//! `job.cancel` should call [`cancel_active`]; it stops between chunks and
//! kills the worker mid-chunk.
use super::{
    Durations, GenRequest, Kind, ModelInfo, Output, PollOutcome, Provider, ProviderError,
    SubmitOutcome,
};
use crate::qts::client::{self, QtsError, SynthOptions};
use crate::qts::models;
use serde_json::Value;

// Re-exported so studio commands (install, clone, preview) reach the engine
// without a direct voice-crate dependency edge.
pub use crate::qts::{client as qts_client, models as qts_models, SAMPLE_RATE_HZ};

pub const ID: &str = "local-qts";
pub const MODEL: &str = "qwen3-tts-0.6b-q8_0";

static MODELS: [ModelInfo; 1] = [ModelInfo {
    provider: ID,
    model: MODEL,
    kind: Kind::Voice,
    label: "Qwen3-TTS 0.6B (local, free) - x-vector cloning; Apple Silicon (Metal) or Vulkan GPU, 8 GB+ RAM; 1.8 GB download",
    est_usd: 0.0,
    durations: Durations::NONE,
}];

/// Stop the synthesis in flight (call from `job.cancel`).
pub fn cancel_active() {
    client::cancel_active();
}

pub struct LocalQts;

impl LocalQts {
    pub fn new() -> Self {
        Self
    }

    fn check(&self, req: &GenRequest) -> Result<(), ProviderError> {
        if req.kind != Kind::Voice {
            return Err(ProviderError::new(
                "invalid_request",
                "local-qts only produces voice",
                false,
            ));
        }
        if req.model != MODEL {
            return Err(ProviderError::new(
                "invalid_request",
                format!("unknown local-qts model {}", req.model),
                false,
            ));
        }
        if req.prompt.trim().is_empty() {
            return Err(ProviderError::new(
                "invalid_request",
                "text is empty",
                false,
            ));
        }
        Ok(())
    }
}

impl Default for LocalQts {
    fn default() -> Self {
        Self::new()
    }
}

fn options(params: &Value) -> Result<SynthOptions, ProviderError> {
    let bad = |m: String| ProviderError::new("invalid_request", m, false);
    let mut o = SynthOptions::default();
    if let Some(preset) = params.get("preset").and_then(Value::as_str) {
        if params.get("voice_id").is_some_and(|v| !v.is_null())
            || params.get("voice_prompt").is_some_and(|v| !v.is_null())
        {
            return Err(bad(
                "preset is an alternative to voice_id/voice_prompt".into()
            ));
        }
        o.mode = client::Mode::CustomVoice;
        o.speaker = Some(preset.to_ascii_lowercase());
    }
    o.instruct = params
        .get("instruct")
        .and_then(Value::as_str)
        .map(String::from);
    if let Some(l) = params
        .get("language")
        .and_then(Value::as_str)
        .filter(|l| !l.is_empty())
    {
        o.language_id =
            client::language_id(l).ok_or_else(|| bad(format!("unsupported language {l}")))?;
    }
    if let Some(t) = params.get("temperature").and_then(Value::as_f64) {
        if !(0.1..=2.0).contains(&t) {
            return Err(bad("temperature must be 0.1-2.0".into()));
        }
        o.temperature = t as f32;
    }
    if let Some(t) = params.get("top_p").and_then(Value::as_f64) {
        if !(0.05..=1.0).contains(&t) {
            return Err(bad("top_p must be 0.05-1.0".into()));
        }
        o.top_p = t as f32;
    }
    if let Some(k) = params.get("top_k").and_then(Value::as_i64) {
        o.top_k = k.clamp(1, 1000) as i32;
    }
    if let Some(c) = params.get("max_chunk_chars").and_then(Value::as_u64) {
        o.max_chunk_chars = (c as usize).clamp(60, 600);
    }
    if let Some(v) = params
        .get("voice_prompt")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        o.voice_prompt = Some(client::resolve_voice_record(v).map_err(map_err)?);
    }
    o.validate().map_err(map_err)?;
    Ok(o)
}

fn map_err(e: QtsError) -> ProviderError {
    let (code, retryable) = match e.code {
        "models_missing" | "models_invalid" | "worker_missing" | "runtime_missing" => {
            ("not_installed", false)
        }
        "voice_missing" | "invalid_request" => ("invalid_request", false),
        "cancelled" => ("cancelled", false),
        "worker_died" | "timeout" => ("unavailable", true),
        other => (other, false),
    };
    ProviderError::new(code, format!("local-qts: {}", e.message), retryable)
}

impl Provider for LocalQts {
    fn id(&self) -> &'static str {
        ID
    }

    fn models(&self) -> &'static [ModelInfo] {
        &MODELS
    }

    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        self.check(req)?;
        // Preserve Base's existing estimate path. New conditioning is checked
        // before queuing jobs, including missing local preset weights.
        if req.params.get("preset").is_some_and(|v| !v.is_null())
            || req.params.get("instruct").is_some_and(|v| !v.is_null())
        {
            let opts = options(&req.params)?;
            if opts.mode == client::Mode::CustomVoice
                && !models::mode_files_present(&models::model_dir_for_mode(opts.mode), opts.mode)
            {
                return Err(ProviderError::new(
                    "not_installed",
                    "model_missing: custom_voice",
                    false,
                ));
            }
        }
        Ok(0.0)
    }

    fn submit(&self, req: &GenRequest, _api_key: &str) -> Result<SubmitOutcome, ProviderError> {
        self.check(req)?;
        let opts = options(&req.params)?;
        let out = client::synthesize(&req.prompt, &opts).map_err(map_err)?;
        Ok(SubmitOutcome::Completed {
            outputs: vec![Output::Bytes {
                bytes: out.wav,
                mime: "audio/wav".into(),
            }],
        })
    }

    fn poll(
        &self,
        _req: &GenRequest,
        _provider_job_id: &str,
        _api_key: &str,
    ) -> Result<PollOutcome, ProviderError> {
        Err(ProviderError::new(
            "invalid_request",
            "local-qts completes synchronously; nothing to poll",
            false,
        ))
    }

    /// `options`: the prompt is the script; the voice comes from `voice_prompt` or `preset`, and `voice_id` is not read.
    fn capabilities(&self, _model: &str) -> crate::capabilities::Capabilities {
        crate::capabilities::Capabilities::default().mode("script")
    }

    fn needs_key(&self) -> bool {
        false
    }
    /// Local engine: a submit has no remote charge, so a replay is free.
    fn submit_is_idempotent(&self) -> bool {
        true
    }

    /// Key check for a keyless local provider: the model files are present
    /// and match the manifest sizes. (Hash verification runs on first use.)
    fn test_key(&self, _api_key: &str) -> Result<(), ProviderError> {
        let dir = models::model_dir();
        if models::files_present(&dir) {
            Ok(())
        } else {
            Err(ProviderError::new(
                "not_installed",
                format!(
                    "QTS model files not found in {} ({} MB download from {})",
                    dir.display(),
                    models::DOWNLOAD_BYTES / 1_000_000,
                    models::HF_REPO
                ),
                false,
            ))
        }
    }

    fn runtime_ready(&self) -> Option<Result<(), ProviderError>> {
        Some(self.test_key(""))
    }

    fn fetch(&self, _output: &Output) -> Result<(Vec<u8>, String), ProviderError> {
        Err(ProviderError::new(
            "invalid_request",
            "local-qts returns bytes directly",
            false,
        ))
    }
}
