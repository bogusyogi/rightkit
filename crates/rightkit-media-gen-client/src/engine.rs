//! Client of the generation engine contract (contract v2, the media-generation engine that
//! `rightkit-media-gen` serves; GenRight's `docs/ENGINE-CLI.md`): `generate`, `job.*`,
//! `asset.resolve`, `models` and `media.*` over the engine CLI. The process call (absolute path,
//! `--input` or stdin, bounded capture, error envelope, contract and capability handshake) is
//! `rightkit_engine_host::call`, the caller side every engine-to-engine call uses; this module adds
//! the generation commands and their typed replies. The engine path is always supplied by the caller.
//!
//! Completion model: `generate` only submits (the Windows detached runner
//! returns in ~160 ms and no longer inherits our stdout), so completion is
//! observed by polling `job status` (`Orchestrator::wait`), never by
//! `generate --wait` and never by process exit of the runner.

use std::sync::Mutex;

pub use rightkit_engine_host::call::{
    EngineInfo, EngineProcess as ProcessRunner, RawOutput, Runner, MAX_CAPTURE_BYTES,
    MAX_INPUT_ARG_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};

/// Contract version this crate speaks (the generation engine's `docs/ENGINE-CLI.md`).
/// Version 1 engines (flattened descriptors, aggregate capability names) are
/// refused with [`Error::ContractVersion`].
pub const REQUIRED_CONTRACT_VERSION: u64 = 2;
/// Capabilities required before any call; contract v2 versions every command.
pub const REQUIRED_CAPABILITIES: [(&str, u32); 7] = [
    ("generate", 1),
    ("job.status", 1),
    ("job.result", 1),
    ("job.list", 1),
    ("job.ack", 1),
    ("asset.resolve", 1),
    ("progress.ndjson", 1),
];

/// Optional `media.*` capabilities: engines advertising these accept
/// `media modalities` / `media generate <kind>` for the widened kind set.
pub const MEDIA_CAPABILITIES: [(&str, u32); 2] = [("media.modalities", 1), ("media.generate", 1)];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Video,
    Voice,
    /// Talking-avatar video (HeyGen, wavespeed infinitetalk).
    Avatar,
    /// Text-to-music (ElevenLabs music_v1, fal stable-audio).
    Music,
    /// Sound effects (elevenlabs sound-generation, fal mmaudio).
    Sfx,
    /// Image/video upscale (fal upscalers, local realesrgan ncnn).
    Upscale,
    /// Re-time an existing video's lips to an audio track (wavespeed
    /// lipsync-3, fal latentsync/sync-lipsync).
    Lipsync,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Voice => "voice",
            Self::Avatar => "avatar",
            Self::Music => "music",
            Self::Sfx => "sfx",
            Self::Upscale => "upscale",
            Self::Lipsync => "lipsync",
        }
    }
}

/// What to generate. Mirrors the engine's `GenRequest` (`kind` is a command word).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerateSpec {
    pub kind: MediaKind,
    pub provider: String,
    pub model: String,
    pub prompt: String,
    #[serde(default)]
    pub params: Value,
}

/// One job record (`job status` / `job list` / `generate` echo).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct JobRecord {
    pub job_id: String,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    pub status: String,
    #[serde(default)]
    pub request: Value,
    #[serde(default)]
    pub provider_job_id: Option<String>,
    #[serde(default)]
    pub attempts: i64,
    #[serde(default)]
    pub est_usd: Option<f64>,
    #[serde(default)]
    pub actual_usd: Option<f64>,
    /// `{code, message}` or null.
    #[serde(default)]
    pub error: Value,
    #[serde(default)]
    pub acked_by: Option<String>,
    #[serde(default)]
    pub acked_at: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    /// Latest progress event (`job status` only).
    #[serde(default)]
    pub progress: Value,
}

impl JobRecord {
    pub fn is_terminal(&self) -> bool {
        matches!(self.status.as_str(), "complete" | "failed" | "cancelled")
    }
}

/// `generate` reply.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GenerateReply {
    pub job_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub deduplicated: bool,
    #[serde(default)]
    pub est_usd: f64,
}

/// One asset of a result descriptor (§4.4). `provider` and `model` are
/// per-asset; they fall back to the descriptor's when absent.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AssetDescriptor {
    pub asset_id: String,
    pub path: String,
    pub sha256: String,
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub mime: String,
    /// `{kind, width, height, duration_s}` from the engine's probe (fields may
    /// be null); passed through untouched.
    #[serde(default)]
    pub media: Value,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

impl AssetDescriptor {
    /// True when the engine's probe left a dimension or duration the media kind
    /// should have as null (unprobed format): the caller should fall back to
    /// ffprobe.
    pub fn needs_probe(&self) -> bool {
        media_needs_probe(&self.media, &self.mime)
    }
}

/// See [`AssetDescriptor::needs_probe`].
pub fn media_needs_probe(media: &Value, mime: &str) -> bool {
    let kind = media["kind"]
        .as_str()
        .or_else(|| mime.split('/').next())
        .unwrap_or("");
    let missing = |k: &str| media[k].is_null();
    match kind {
        "image" => missing("width") || missing("height"),
        "video" => missing("width") || missing("height") || missing("duration_s"),
        "voice" | "audio" => missing("duration_s"),
        _ => true,
    }
}

/// `job result` descriptor. Only `assets` is read; the top-level
/// single-asset flattening of older engines is ignored.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ResultDescriptor {
    pub job_id: String,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub acked_by: Option<String>,
    #[serde(default)]
    pub acked_at: Option<String>,
    pub assets: Vec<AssetDescriptor>,
}

/// `asset resolve` reply: where an asset lives now (works after a library
/// move).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AssetResolution {
    pub asset_id: String,
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub mime: String,
    #[serde(default)]
    pub exists: bool,
}

/// `job list` filter.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JobListQuery {
    pub request_id: Option<String>,
    pub status: Option<String>,
    /// Engine default 50, clamped to 1..=500.
    pub limit: Option<u32>,
}

/// `job list` page. `truncated` is true when more jobs exist than returned
/// (inferred when the engine does not say and the page is full).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JobPage {
    pub jobs: Vec<JobRecord>,
    pub truncated: bool,
}

type Sink = Box<dyn FnMut(Value) + Send>;

/// Typed wrapper over [`Runner`]. Forwards NDJSON progress lines found on the
/// engine's stderr to the event sink.
pub struct GenerationClient<R: Runner> {
    runner: R,
    sink: Mutex<Option<Sink>>,
}

impl<R: Runner> GenerationClient<R> {
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            sink: Mutex::new(None),
        }
    }

    pub fn runner(&self) -> &R {
        &self.runner
    }

    /// Install the host NDJSON event sink.
    pub fn set_event_sink(&self, sink: impl FnMut(Value) + Send + 'static) {
        *self.sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(sink));
    }

    pub fn emit(&self, event: Value) {
        if let Some(s) = self.sink.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            s(event);
        }
    }

    fn call(&self, words: &[&str], input: Option<&Value>) -> Result<Value> {
        let label = words.join(" ");
        let text = input.map(Value::to_string);
        let raw = self.runner.run(words, text.as_deref())?;
        for line in rightkit_engine_host::call::json_lines(&raw.stderr) {
            self.emit(progress_event(&line, None, Some(&label)));
        }
        Ok(rightkit_engine_host::call::parse_reply(&label, &raw)?)
    }

    fn typed<T: for<'de> Deserialize<'de>>(
        &self,
        words: &[&str],
        input: Option<&Value>,
    ) -> Result<T> {
        let v = self.call(words, input)?;
        serde_json::from_value(v).map_err(|e| Error::BadOutput {
            command: words.join(" "),
            detail: e.to_string(),
        })
    }

    /// `doctor`, validated: contract version 2 and every required capability.
    pub fn check_capabilities(&self) -> Result<EngineInfo> {
        let report = self.call(&["doctor"], None)?;
        Ok(rightkit_engine_host::call::handshake(
            &report,
            REQUIRED_CONTRACT_VERSION,
            &REQUIRED_CAPABILITIES,
        )?)
    }

    /// `models`: the engine's catalog (provider, model, kind, label, est_usd,
    /// and the durations read from the label).
    pub fn models(&self) -> Result<crate::models::ModelCatalog> {
        Ok(crate::models::ModelCatalog::from_value(
            &self.call(&["models"], None)?,
        ))
    }

    /// `models` from an engine advertising `models.durations`: the structured
    /// duration fields are authoritative and labels are not parsed.
    pub fn models_structured(&self) -> Result<crate::models::ModelCatalog> {
        Ok(crate::models::ModelCatalog::from_value_structured(
            &self.call(&["models"], None)?,
        ))
    }

    /// `generate <kind>` with a caller request id (the engine maps one request
    /// id to exactly one job).
    pub fn generate(&self, spec: &GenerateSpec, request_id: &str) -> Result<GenerateReply> {
        let input = json!({
            "provider": spec.provider,
            "model": spec.model,
            "prompt": spec.prompt,
            "params": if spec.params.is_null() { json!({}) } else { spec.params.clone() },
            "request_id": request_id,
        });
        self.typed(&["generate", spec.kind.as_str()], Some(&input))
    }

    /// `media modalities`: the honest capability catalog (per-kind
    /// `available`/`unconfigured`/`unsupported` state plus per-provider
    /// credential/installed/readiness verdicts). Use before offering a kind
    /// the engine cannot route.
    pub fn media_modalities(&self, probe: bool) -> Result<Value> {
        self.call(&["media", "modalities"], Some(&json!({"probe": probe})))
    }

    /// `media generate <kind>`: like `generate`, but `provider` may be
    /// omitted (inferred from a unique model) and kind validation produces
    /// typed `unknown_kind`/`unsupported_kind` errors.
    pub fn media_generate(&self, spec: &GenerateSpec, request_id: &str) -> Result<GenerateReply> {
        let mut input = json!({
            "model": spec.model,
            "prompt": spec.prompt,
            "params": if spec.params.is_null() { json!({}) } else { spec.params.clone() },
            "request_id": request_id,
        });
        // `provider` is optional on the media surface.
        if !spec.provider.is_empty() {
            input["provider"] = json!(spec.provider);
        }
        self.typed(&["media", "generate", spec.kind.as_str()], Some(&input))
    }

    pub fn job_status(&self, job_id: &str) -> Result<JobRecord> {
        self.typed(&["job", "status", job_id], None)
    }

    pub fn job_result(&self, job_id: &str) -> Result<ResultDescriptor> {
        self.typed(&["job", "result", job_id], None)
    }

    /// Recent jobs (the engine clamps `limit` to 1..=500). Prefer
    /// [`Self::job_page`], which reports truncation.
    pub fn job_list(&self, limit: u32) -> Result<Vec<JobRecord>> {
        Ok(self
            .job_page(&JobListQuery {
                limit: Some(limit),
                ..JobListQuery::default()
            })?
            .jobs)
    }

    /// `job list {request_id?, status?, limit?}`. Engines that predate the
    /// filters ignore them, so callers must still match rows themselves.
    pub fn job_page(&self, q: &JobListQuery) -> Result<JobPage> {
        let mut input = serde_json::Map::new();
        if let Some(r) = &q.request_id {
            input.insert("request_id".into(), json!(r));
        }
        if let Some(s) = &q.status {
            input.insert("status".into(), json!(s));
        }
        if let Some(l) = q.limit {
            input.insert("limit".into(), json!(l));
        }
        let v = self.call(&["job", "list"], Some(&Value::Object(input)))?;
        let jobs: Vec<JobRecord> =
            serde_json::from_value(v["jobs"].clone()).map_err(|e| Error::BadOutput {
                command: "job list".into(),
                detail: e.to_string(),
            })?;
        let limit = q.limit.unwrap_or(50).clamp(1, 500) as usize;
        let truncated = v["truncated"].as_bool().unwrap_or(jobs.len() >= limit);
        Ok(JobPage { jobs, truncated })
    }

    /// `job status --request-id <id>`. `Ok(None)` is the engine's authoritative
    /// `job_not_found`.
    pub fn job_status_by_request(&self, request_id: &str) -> Result<Option<JobRecord>> {
        match self.typed(
            &["job", "status", "--request-id", request_id],
            Some(&json!({"request_id": request_id})),
        ) {
            Ok(rec) => Ok(Some(rec)),
            Err(Error::Engine { code, .. }) if code == "job_not_found" => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `job status <id>` with `{"events":true}`: the live record plus the
    /// persisted progress log (oldest first, last 200).
    pub fn job_status_events(&self, job_id: &str) -> Result<(JobRecord, Vec<Value>)> {
        let v = self.call(&["job", "status", job_id], Some(&json!({"events": true})))?;
        let events = v["events"].as_array().cloned().unwrap_or_default();
        let rec = serde_json::from_value(v).map_err(|e| Error::BadOutput {
            command: "job status".into(),
            detail: e.to_string(),
        })?;
        Ok((rec, events))
    }

    /// Just the progress log of a job.
    pub fn job_events(&self, job_id: &str) -> Result<Vec<Value>> {
        Ok(self.job_status_events(job_id)?.1)
    }

    /// `asset resolve <asset_id>`. `Ok(None)` is `asset_not_found`; an
    /// `asset_corrupt` engine error (moved file fails its sha256) is returned
    /// as `Error::Engine`.
    pub fn asset_resolve(&self, asset_id: &str) -> Result<Option<AssetResolution>> {
        match self.typed(&["asset", "resolve", asset_id], None) {
            Ok(r) => Ok(Some(r)),
            Err(Error::Engine { code, .. }) if code == "asset_not_found" => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `job ack <id>`: the result is durably imported; the engine may release it.
    pub fn job_ack(&self, job_id: &str, consumer: &str) -> Result<JobRecord> {
        self.typed(
            &["job", "ack", job_id],
            Some(&json!({"consumer": consumer})),
        )
    }

    /// `job cancel <id>` (capability `job.cancel`): asks the engine to stop a
    /// running job; a finished job's assets are never removed.
    pub fn job_cancel(&self, job_id: &str) -> Result<JobRecord> {
        self.typed(&["job", "cancel", job_id], None)
    }
}

/// Map one NDJSON progress object of the engine to a host event.
/// `job.progress` lines become `media_gen.job_progress` with the typed fields
/// lifted; anything else stays a generic `media_gen.progress`.
pub fn progress_event(line: &Value, request_id: Option<&str>, command: Option<&str>) -> Value {
    if line["event"] == "job.progress" {
        let mut ev = json!({
            "event": "media_gen.job_progress",
            "job_id": line["job_id"], "status": line["status"], "stage": line["stage"],
            "attempt": line["attempt"], "at": line["at"],
        });
        if !line["message"].is_null() {
            ev["message"] = line["message"].clone();
        }
        if let Some(r) = request_id {
            ev["request_id"] = json!(r);
        }
        return ev;
    }
    json!({"event": "media_gen.progress", "command": command, "data": line})
}
