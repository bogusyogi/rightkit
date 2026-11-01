//! Provider adapters. One Rust adapter per provider (§4.2). Each adapter
//! takes its base URL in its constructor so tests run against a local mock;
//! [`registry`] wires production URLs.

pub mod elevenlabs;
pub mod evolink;
pub mod fal;
pub mod google;
pub mod heygen;
pub mod higgsfield;
pub mod local_blender;
pub mod local_image;
pub mod local_lipsync;
pub mod local_wan2gp;
pub mod muapi;
pub mod musicgen;
pub mod nanobanana;
pub mod openai;
pub mod qts;
pub mod realesrgan;
pub mod replicate;
pub mod wavespeed;

/// The organisation root for this OS: where installed local tools live. It is the organisation
/// folder every RightKit app nests under, so it is the parent of any app's resolved `local` folder
/// (`rightkit_fs::AppDirs`: `%LOCALAPPDATA%`, `~/Library/Application Support`, or the XDG data
/// folder, each joined with the organisation name). The app name only supplies the child that is
/// dropped.
pub fn orthic_root() -> Option<std::path::PathBuf> {
    let dirs = rightkit_fs::AppDirs::resolve(rightkit_fs::AppId::new("RightKit")).ok()?;
    Some(dirs.local.parent()?.to_path_buf())
}

use crate::host::http::HttpError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Image,
    Video,
    Voice,
    /// Talking-avatar video (HeyGen and similar).
    Avatar,
    /// Text-to-music tracks (ElevenLabs Music, Stable Audio).
    Music,
    /// Text- or video-driven sound effects (ElevenLabs sound-generation,
    /// fal MMAudio video-to-audio).
    Sfx,
    /// Image or video upscaling (fal upscalers, local Real-ESRGAN ncnn).
    Upscale,
    /// Re-time a video's lip movements to an audio track (sync-lipsync,
    /// LatentSync, WaveSpeed lipsync-3). Distinct from `Avatar` (which
    /// animates a still image) so consumers pick the right input contract.
    Lipsync,
    /// Plain text in, text out (LLM chat endpoints such as fal any-llm).
    /// Powers translation and other language steps; the artifact is the
    /// response text stored as `text/plain` bytes.
    Text,
    /// Continue an existing video past its last frame.
    Extend,
    /// Drive a character image with the motion of a reference video.
    MotionControl,
    /// Keep a source video and swap one targeted element for a reference image.
    ReplaceObject,
    /// Re-render a source video in another style.
    Restyle,
    /// Speech in, transcript out: text plus timed segments (ElevenLabs Scribe v2 on fal and
    /// WaveSpeed). The artifact is the transcript JSON stored as `application/json` bytes; see
    /// [`transcript_output`].
    Transcribe,
}

/// A pause this long (seconds) between words starts a new transcript segment.
const SEGMENT_GAP_S: f64 = 1.0;

/// One timed run of words in a transcript.
struct Segment {
    start: f64,
    end: f64,
    text: String,
    speaker: Option<String>,
}

/// The transcript result shape shared by every transcription route:
/// `{"text", "language_code"?, "segments": [{"start", "end", "text", "speaker"}]}`, times in
/// seconds. `reply` is a provider reply with a full `text` and an optional `words` array of
/// `{text, start, end, type?, speaker_id?}` (`type` other than `word`, such as `spacing`, is
/// skipped). Segments group consecutive words and break on a speaker change or a pause of
/// [`SEGMENT_GAP_S`] or more. Returns `None` when the reply has no non-empty text.
pub(crate) fn transcript_output(reply: &Value) -> Option<Output> {
    let text = reply
        .get("text")
        .and_then(Value::as_str)?
        .trim()
        .to_string();
    if text.is_empty() {
        return None;
    }
    let mut segs: Vec<Segment> = Vec::new();
    for w in reply
        .get("words")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        if w.get("type").and_then(Value::as_str).unwrap_or("word") != "word" {
            continue;
        }
        let (Some(start), Some(end), Some(t)) = (
            w.get("start").and_then(Value::as_f64),
            w.get("end").and_then(Value::as_f64),
            w.get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty()),
        ) else {
            continue;
        };
        let speaker = w
            .get("speaker_id")
            .and_then(Value::as_str)
            .map(String::from);
        let joins =
            matches!(segs.last(), Some(s) if start - s.end < SEGMENT_GAP_S && s.speaker == speaker);
        if joins {
            if let Some(s) = segs.last_mut() {
                s.end = end;
                s.text.push(' ');
                s.text.push_str(t);
            }
        } else {
            segs.push(Segment {
                start,
                end,
                text: t.to_string(),
                speaker,
            });
        }
    }
    let segments: Vec<Value> = segs
        .iter()
        .map(|s| serde_json::json!({"start": s.start, "end": s.end, "text": s.text, "speaker": s.speaker}))
        .collect();
    let mut out = serde_json::json!({"text": text, "segments": segments});
    if let Some(lang) = reply.get("language_code").and_then(Value::as_str) {
        out["language_code"] = serde_json::json!(lang);
    }
    let bytes = serde_json::to_vec(&out).ok()?;
    Some(Output::Bytes {
        bytes,
        mime: "application/json".into(),
    })
}

/// One generation request as recorded in the job store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenRequest {
    pub kind: Kind,
    pub provider: String,
    pub model: String,
    pub prompt: String,
    /// Provider/model specific options (aspect, duration, voice_id, image_url…).
    #[serde(default)]
    pub params: Value,
}

/// A produced media item before the host stores it in its library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Output {
    Url {
        url: String,
        mime: Option<String>,
    },
    Bytes {
        bytes: Vec<u8>,
        mime: String,
    },
    /// Finalized artifact owned by a local worker. The library verifies and
    /// adopts it into immutable storage without loading the whole file.
    Path {
        path: std::path::PathBuf,
        mime: String,
        sha256: String,
        bytes: u64,
    },
    /// File finalized by a local provider in its own content tree (the
    /// provider's [`Provider::finalized_root`]). The library keeps that file in
    /// place and stores a clone/link of it, paired for deletion, so the output
    /// costs no second copy of the bytes.
    Finalized {
        path: std::path::PathBuf,
        mime: String,
        sha256: String,
        bytes: u64,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SubmitOutcome {
    /// Asynchronous provider accepted the job; poll with this id.
    Accepted { provider_job_id: String },
    /// Synchronous provider returned the result directly.
    Completed { outputs: Vec<Output> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PollOutcome {
    Running,
    Completed {
        outputs: Vec<Output>,
    },
    /// The provider CONFIRMED the job failed, in its own documented vocabulary, with no charge
    /// (each adapter lists the exact statuses it treats this way). Nothing is owed: the
    /// reservation is released. Only an idempotent adapter's job is resubmitted automatically;
    /// a paid, non-idempotent job fails and the user retries explicitly (§4.3).
    Failed {
        message: String,
    },
    /// The provider answered, but not with anything the adapter can classify: an unrecognised
    /// status string, a completed job with no usable output, a 422 on the result fetch, an
    /// unparseable body. The job MAY still be running or charged, so it is held as
    /// `outcome_unknown` with its reservation kept and is never resubmitted automatically.
    /// Build it with [`unknown_outcome`].
    Unknown {
        /// The raw status (or short label) the provider gave.
        status: String,
        /// Clipped, key-redacted excerpt of the reply, stored for the user.
        detail: String,
        /// The provider says the work itself finished (COMPLETED without usable output): it did
        /// the work, so the reservation is settled at the estimate instead of staying open.
        completed: bool,
    },
}

/// Longest reply excerpt stored with an [`PollOutcome::Unknown`].
const UNKNOWN_EXCERPT_CHARS: usize = 300;

/// An excerpt of a provider reply that is safe to store and show: lossy UTF-8, whitespace
/// collapsed, the API key replaced, cut to [`UNKNOWN_EXCERPT_CHARS`] characters.
pub fn clip_excerpt(body: &[u8], api_key: &str) -> String {
    let text = String::from_utf8_lossy(body);
    let text = if api_key.is_empty() {
        text.into_owned()
    } else {
        text.replace(api_key, "[redacted]")
    };
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = collapsed.chars().take(UNKNOWN_EXCERPT_CHARS).collect();
    if collapsed.chars().count() > UNKNOWN_EXCERPT_CHARS {
        out.push_str("...");
    }
    out
}

/// A poll reply the adapter cannot classify; see [`PollOutcome::Unknown`]. `status` is clipped
/// and key-redacted like the body excerpt.
pub fn unknown_outcome(status: &str, body: &[u8], api_key: &str, completed: bool) -> PollOutcome {
    PollOutcome::Unknown {
        status: clip_excerpt(status.as_bytes(), api_key)
            .chars()
            .take(64)
            .collect(),
        detail: clip_excerpt(body, api_key),
        completed,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderError {
    pub code: String,
    pub message: String,
    /// Transport or 5xx failures; the job stays recoverable.
    pub retryable: bool,
    /// Set only on a failed submit: the request may have reached the
    /// provider, so a paid job may exist remotely. A failure that provably
    /// sent nothing, or that the provider definitely rejected, leaves it false.
    pub ambiguous: bool,
}

impl ProviderError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
            ambiguous: false,
        }
    }

    /// Mark the submit as possibly delivered.
    pub fn ambiguous(mut self) -> Self {
        self.ambiguous = true;
        self
    }

    /// Mark the submit as possibly delivered when `cond` holds.
    pub fn ambiguous_if(mut self, cond: bool) -> Self {
        self.ambiguous |= cond;
        self
    }

    /// For an error mapped from a reply that did arrive: its `retryable`
    /// statuses (408, 429, 5xx) leave the outcome open, every other status is
    /// a definite rejection.
    pub fn ambiguous_if_retryable(self) -> Self {
        let retryable = self.retryable;
        self.ambiguous_if(retryable)
    }
}

/// Classify a failed ONNX Runtime initialisation (`crate::qts::models::init_onnxruntime`).
/// Library file absent (no resolvable candidate) → `not_installed`, retryable, so the UI keeps its
/// install action and a job pauses until the install lands. Library file present but refused by
/// the OS loader → `runtime_load_failed`, non-retryable. The decision is a plain `is_file` check on
/// the resolved path; the loader's text is only quoted, never parsed.
pub fn ort_init_failure(reason: &str) -> ProviderError {
    match crate::qts::models::resolved_onnxruntime() {
        Some(path) if path.is_file() => runtime_load_failed(&path, reason),
        _ => ProviderError::new(
            "not_installed",
            format!("ONNX Runtime shared library not found: {reason}"),
            true,
        ),
    }
}

/// A runtime library that exists but could not be loaded. Non-retryable: the same file fails again.
pub fn runtime_load_failed(path: &std::path::Path, reason: &str) -> ProviderError {
    let mut message = format!(
        "ONNX Runtime library {} is present but could not be loaded: {reason}.",
        path.display()
    );
    if cfg!(target_os = "macos") {
        message.push_str(" The signed engine may not load this library (library validation).");
    }
    ProviderError::new("runtime_load_failed", message, false)
}

/// Session-open failure on a model file that is present (corrupt file, unsupported opset, ...).
/// Non-retryable: retrying the same file cannot succeed.
pub fn model_load_failed(path: &std::path::Path, reason: impl std::fmt::Display) -> ProviderError {
    ProviderError::new(
        "model_load_failed",
        format!("{}: {reason}", path.display()),
        false,
    )
}

/// Whether a failed send may have delivered the request. Uses the delivery evidence
/// rightkit-http reports through `on_attempt_outcome` for the call (kept by
/// `crate::host::http::Http::send`): the monotonic maximum over every hop, so a
/// redirect chain that responded once is never made "safe" by a later refused hop. Any
/// delivery other than `NotSent` holds the job as `outcome_unknown` (never auto-resubmitted).
/// With no hop observed, only the pre-send refusals say no; any other error is treated as
/// possibly delivered. A completed exchange is not a send failure (its status is judged by
/// [`status_may_have_run`]).
// RightKit ruling 12b1a381 #1: composition of rightkit-http 0.3.6 `Client::on_attempt_outcome`.
pub(crate) fn request_may_have_reached<T>(sent: &Result<T, HttpError>) -> bool {
    match sent {
        Ok(_) => {
            // Clear the evidence of a completed exchange so it never leaks into the next call on this thread.
            let _ = crate::host::http::take_delivery();
            false
        }
        Err(e) => error_may_have_reached(e),
    }
}

/// [`request_may_have_reached`] for an error already in hand (callers that need the error afterwards, such as the
/// rented-worker driver deciding whether to hold or release a billed reservation). The monotonic hop evidence of the
/// last `Http::send` on this thread wins over the error variant: an `InvalidRequest` or `AddressDenied` raised by a
/// later redirect hop never turns an earlier `Responded` hop into "not sent".
pub fn error_may_have_reached(e: &HttpError) -> bool {
    use crate::host::http::{take_delivery, Delivery};
    match take_delivery() {
        Some(Delivery::NotSent) => false,
        Some(_) => true,
        None => !matches!(
            e,
            HttpError::InvalidRequest(_) | HttpError::AddressDenied { .. } | HttpError::Cancelled
        ),
    }
}

/// Whether a non-2xx reply to a submit leaves the outcome open: a server
/// error, a request timeout (408) or a rate limit (429) may follow work the
/// provider already accepted. Any other 4xx (auth, billing, invalid request)
/// is a definite rejection.
pub(crate) fn status_may_have_run(status: u16) -> bool {
    matches!(status, 408 | 429 | 500..=599)
}

/// Static description used by `doctor`, onboarding, and the UI.
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub provider: &'static str,
    pub model: &'static str,
    pub kind: Kind,
    pub label: &'static str,
    /// Estimate for one default-sized output, used to reserve spend.
    pub est_usd: f64,
    /// Accepted output durations; omitted when the model takes none.
    #[serde(flatten)]
    pub durations: Durations,
}

/// Duration rules advertised by `models` (capability `models.durations`):
/// `durations_s` is a fixed set (always an array, even with one value);
/// `min_duration_s`/`max_duration_s` bound a range.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Durations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durations_s: Option<&'static [u32]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_duration_s: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_duration_s: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_duration_s: Option<u32>,
}

impl Durations {
    pub const NONE: Self = Self {
        durations_s: None,
        min_duration_s: None,
        max_duration_s: None,
        default_duration_s: None,
    };
    pub const fn set(values: &'static [u32], default: u32) -> Self {
        Self {
            durations_s: Some(values),
            min_duration_s: None,
            max_duration_s: None,
            default_duration_s: Some(default),
        }
    }
    pub const fn range(min: u32, max: u32, default: Option<u32>) -> Self {
        Self {
            durations_s: None,
            min_duration_s: Some(min),
            max_duration_s: Some(max),
            default_duration_s: default,
        }
    }
}

/// Reference images an image model accepts, as its adapter reads them today. Serialised as the
/// `references` field of the model's `models` / `media modalities` row, and only when present.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct ReferenceInput {
    /// Request param holding the list of reference URLs (`params.<param>`, an array of strings).
    pub param: &'static str,
    /// Largest list the adapter validates; omitted when the adapter validates no count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
    /// `fal_storage`: a local file is uploaded to the user's fal storage first, and only with the
    /// run's `confirm_fal_upload: true`. `none`: only public https URLs are accepted.
    pub local_upload: &'static str,
}

pub trait Provider: Send + Sync {
    /// Stable id, also the keychain account name (e.g. "fal", "elevenlabs").
    fn id(&self) -> &'static str;
    fn models(&self) -> &'static [ModelInfo];
    /// Reference-image input for one of this provider's image models; `None` when the adapter
    /// takes no reference images for it.
    fn references(&self, _model: &str) -> Option<ReferenceInput> {
        None
    }
    /// Input mode and settings of one of this provider's models, as the adapter's own validation
    /// enforces them (see `crate::capabilities`). Listed on the `models` rows as `inputs` and
    /// `settings`; empty when the adapter states nothing.
    fn capabilities(&self, _model: &str) -> crate::capabilities::Capabilities {
        crate::capabilities::Capabilities::default()
    }
    /// Cost estimate for this request in USD; reserved before submission.
    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError>;
    fn submit(&self, req: &GenRequest, api_key: &str) -> Result<SubmitOutcome, ProviderError>;
    /// Stable engine identity for local adapters that persist their own queue.
    /// `idempotency_key` is the job layer's stable key for this attempt
    /// (`<job_id>:<attempt>`): the same value on a replay of the same attempt.
    fn submit_job(
        &self,
        req: &GenRequest,
        api_key: &str,
        _job_id: &str,
        _attempt: i64,
        _idempotency_key: &str,
    ) -> Result<SubmitOutcome, ProviderError> {
        self.submit(req, api_key)
    }
    /// True only when sending the same attempt twice cannot create a second
    /// remote operation or charge: the adapter sends `idempotency_key` in a
    /// way the provider's API is documented to honour, or the submit has no
    /// remote effect (a local engine). When false, the job layer never
    /// re-sends an attempt whose first send may have been delivered: the job
    /// becomes `outcome_unknown` instead.
    fn submit_is_idempotent(&self) -> bool {
        false
    }
    /// True only when [`Provider::cancel_job`] really asks the provider to stop the work and
    /// its `Ok` means the provider confirmed it. A keyed (paid) adapter that returns false has
    /// no cancel endpoint: cancelling its accepted job cannot stop billing, so the job is held
    /// with its reservation instead of released. Local engines are not consulted.
    fn can_cancel(&self) -> bool {
        false
    }
    /// Release provider work after engine cancellation, including a submit/id-save race.
    fn cancel_job(
        &self,
        _req: &GenRequest,
        _provider_job_id: Option<&str>,
        _job_id: &str,
    ) -> Result<(), ProviderError> {
        Ok(())
    }
    fn progress_message(&self, _req: &GenRequest, _provider_job_id: &str) -> Option<String> {
        None
    }
    fn poll(
        &self,
        req: &GenRequest,
        provider_job_id: &str,
        api_key: &str,
    ) -> Result<PollOutcome, ProviderError>;
    /// A free, non-generating call proving the key works (onboarding step 4).
    fn test_key(&self, api_key: &str) -> Result<(), ProviderError>;
    /// Whether generation requires a stored API key. Local engines (QTS,
    /// local worker) are keyless: their `test_key` checks installation instead.
    fn needs_key(&self) -> bool {
        true
    }
    /// Cheap runtime-readiness probe for keyless local providers: files and
    /// manifests on disk only — never launches a worker, never hits the
    /// network. `None` for keyed providers (credential state is the gate) or
    /// when the provider has no meaningful local runtime to check. Status
    /// surfaces use this so "keyless" is never read as "ready".
    fn runtime_ready(&self) -> Option<Result<(), ProviderError>> {
        None
    }
    /// True when the "local" runtime is an external scripted worker (e.g. a
    /// Python venv launched on demand) rather than code shipped in the app.
    /// External workers are a supported route but must never be reported as
    /// bundled/native readiness.
    fn runtime_is_external(&self) -> bool {
        false
    }
    /// Download an [`Output::Url`]; returns bytes and mime type. Path outputs
    /// are adopted by the library and must never be fetched here.
    fn fetch(&self, output: &Output) -> Result<(Vec<u8>, String), ProviderError>;
    /// Content root a provider's [`Output::Finalized`] files must live under.
    /// Providers that never return `Finalized` keep the default.
    fn finalized_root(&self) -> Option<std::path::PathBuf> {
        None
    }
}

/// True only in a `test-mode` build (or this crate's own unit tests): provider base-URL overrides are
/// honoured nowhere else, because an override can send a real API key to another host.
pub const PROVIDER_BASE_OVERRIDES: bool = cfg!(any(test, feature = "test-mode"));

/// The base URL for a provider endpoint. In a `test-mode` build, `var` (for
/// example `RIGHTKIT_FAL_REST_BASE` or `VAST_API_BASE`) replaces `default`; this
/// is how the e2e suite points adapters at a local mock to assert exact request
/// shapes without a billed call. In any other build the variable is ignored and
/// `default` is returned. The result has no trailing slash.
pub fn base_url_override(var: &str, default: &str) -> String {
    if PROVIDER_BASE_OVERRIDES {
        let set = std::env::var(var)
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty());
        if let Some(url) = set {
            return url;
        }
    }
    default.trim_end_matches('/').to_string()
}

/// True when a Rust adapter table already defines this model; a declared
/// catalogue entry cannot redefine it.
pub(crate) fn is_builtin(provider: &str, model: &str) -> bool {
    match provider {
        "fal" => fal::is_builtin(model),
        "wavespeed" => wavespeed::is_builtin(model),
        _ => false,
    }
}

/// Production adapters.
pub fn registry() -> Vec<Box<dyn Provider>> {
    vec![
        Box::new(fal::Fal::new(
            &base_url_override("RIGHTKIT_FAL_QUEUE_BASE", fal::QUEUE_BASE),
            &base_url_override("RIGHTKIT_FAL_REST_BASE", fal::REST_BASE),
        )),
        Box::new(elevenlabs::ElevenLabs::new(&base_url_override(
            "RIGHTKIT_ELEVENLABS_BASE",
            elevenlabs::BASE,
        ))),
        Box::new(nanobanana::NanoBanana::new(&base_url_override(
            "RIGHTKIT_NANOBANANA_BASE",
            nanobanana::BASE,
        ))),
        Box::new(wavespeed::WaveSpeed::new(&base_url_override(
            "RIGHTKIT_WAVESPEED_BASE",
            wavespeed::BASE,
        ))),
        Box::new(heygen::HeyGen::new(&base_url_override(
            "RIGHTKIT_HEYGEN_BASE",
            heygen::BASE,
        ))),
        Box::new(qts::LocalQts::new()),
        Box::new(local_image::LocalImage::new()),
        Box::new(local_blender::LocalBlender::new()),
        Box::new(local_lipsync::LocalLipsync::new()),
        Box::new(local_wan2gp::LocalWan2Gp::new()),
        Box::new(realesrgan::RealEsrgan::new()),
        Box::new(musicgen::LocalMusicGen::new()),
        Box::new(evolink::Evolink::new(&base_url_override(
            "RIGHTKIT_EVOLINK_BASE",
            evolink::BASE,
        ))),
        Box::new(openai::OpenAi::new(&base_url_override(
            "RIGHTKIT_OPENAI_BASE",
            openai::BASE,
        ))),
        Box::new(google::Google::new(&base_url_override(
            "RIGHTKIT_GOOGLE_BASE",
            google::BASE,
        ))),
        Box::new(replicate::Replicate::new(&base_url_override(
            "RIGHTKIT_REPLICATE_BASE",
            replicate::BASE,
        ))),
        Box::new(higgsfield::Higgsfield::new(&base_url_override(
            "RIGHTKIT_HIGGSFIELD_BASE",
            higgsfield::BASE,
        ))),
        Box::new(muapi::Muapi::new(&base_url_override(
            "RIGHTKIT_MUAPI_BASE",
            muapi::BASE,
        ))),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str) -> serde_json::Value {
        let all: Vec<_> = registry().iter().flat_map(|p| p.models().iter()).collect();
        serde_json::to_value(all.into_iter().find(|m| m.model == id).unwrap()).unwrap()
    }

    #[test]
    fn unknown_outcome_excerpt_is_clipped_and_key_redacted() {
        let body = format!("{{\"detail\": \"key sk-secret-1 {}\"}}", "x".repeat(1000));
        let PollOutcome::Unknown {
            status,
            detail,
            completed,
        } = unknown_outcome("WEIRD", body.as_bytes(), "sk-secret-1", true)
        else {
            panic!()
        };
        assert_eq!((status.as_str(), completed), ("WEIRD", true));
        assert!(
            !detail.contains("sk-secret-1") && detail.contains("[redacted]"),
            "{detail}"
        );
        assert!(
            detail.chars().count() <= 303,
            "clipped: {}",
            detail.chars().count()
        );
    }

    #[test]
    fn models_advertise_structured_durations() {
        let veo = model("fal-ai/veo3.1/fast");
        assert_eq!(veo["durations_s"], serde_json::json!([4, 6, 8]));
        assert_eq!(veo["default_duration_s"], 4);
        assert!(veo.get("min_duration_s").is_none());
        let kling = model("kling-3-pro");
        assert_eq!(
            (
                kling["min_duration_s"].as_u64(),
                kling["max_duration_s"].as_u64()
            ),
            (Some(3), Some(15))
        );
        assert!(kling.get("durations_s").is_none());
        assert_eq!(model("wan-ultra")["durations_s"], serde_json::json!([5, 8]));
        let image = model("fal-ai/flux-2/klein/9b");
        assert!(image.get("durations_s").is_none() && image.get("min_duration_s").is_none());
    }
}
