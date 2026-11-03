//! Process boundary to the installed `genright-engine` (contract v2, see
//! GenRight `docs/ENGINE-CLI.md`): JSON in via `--input '<json>'` (or
//! `--input -` plus stdin for large payloads), one JSON value out on stdout,
//! NDJSON progress (if any) on stderr, `{error:{code,message}}` on failure.
//! The engine reads stdin ONLY with `--input -`, so every other call gets a
//! null stdin and can never block on an inherited or open pipe. The engine
//! path is always supplied by the caller.
//!
//! Completion model: `generate` only submits (the Windows detached runner
//! returns in ~160 ms and no longer inherits our stdout), so completion is
//! observed by polling `job status` (`Orchestrator::wait`), never by
//! `generate --wait` and never by process exit of the runner.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use rightkit_child_process::{NativeOutputCapture, OwnedCommand};
use rightkit_job_queue::parse_doctor;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};

/// Contract version this crate speaks (GenRight `docs/ENGINE-CLI.md`).
/// Version 1 engines (flattened descriptors, aggregate capability names) are
/// refused with [`Error::ContractVersion`].
pub const REQUIRED_CONTRACT_VERSION: u64 = 2;
/// Capabilities required before any call; contract v2 versions every command.
/// Largest JSON payload passed on the command line; bigger ones go through
/// `--input -` and stdin (Windows command lines top out near 32k characters
/// and JSON quoting can double the size).
pub const MAX_INPUT_ARG_BYTES: usize = 8 * 1024;
/// Ceiling on one captured stream. Output past it is refused as incomplete.
pub const MAX_CAPTURE_BYTES: usize = 64 * 1024 * 1024;
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

/// What to generate. Mirrors GenRight's `GenRequest` (`kind` is a command word).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerateSpec {
    pub kind: MediaKind,
    pub provider: String,
    pub model: String,
    pub prompt: String,
    #[serde(default)]
    pub params: Value,
}

/// Raw result of one engine invocation.
#[derive(Debug, Clone, Default)]
pub struct RawOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: Option<i32>,
}

/// Runs one `genright-engine` command.
pub trait Runner: Send + Sync {
    fn run(&self, words: &[&str], input: Option<&str>) -> Result<RawOutput>;
}

/// Spawns the engine binary at an explicit path.
#[derive(Debug, Clone)]
pub struct ProcessRunner {
    engine: PathBuf,
    timeout: Duration,
}

impl ProcessRunner {
    pub fn new(engine: impl Into<PathBuf>) -> Self {
        Self {
            engine: engine.into(),
            timeout: Duration::from_secs(60),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn engine(&self) -> &Path {
        &self.engine
    }
}

impl Runner for ProcessRunner {
    fn run(&self, words: &[&str], input: Option<&str>) -> Result<RawOutput> {
        let label = words.join(" ");
        let via_stdin = input.is_some_and(|i| i.len() > MAX_INPUT_ARG_BYTES);
        let mut cmd = OwnedCommand::new(&self.engine);
        cmd.command_mut().args(words);
        match input {
            Some(_) if via_stdin => {
                cmd.command_mut().args(["--input", "-"]);
            }
            Some(i) => {
                cmd.command_mut().args(["--input", i]);
            }
            None => {}
        }
        cmd.command_mut()
            .stdin(if via_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|source| Error::Spawn {
            path: self.engine.clone(),
            source,
        })?;
        // Drain both streams before anything blocks on the child.
        let capture_error = |source| Error::Spawn {
            path: self.engine.clone(),
            source,
        };
        let mut stdout = NativeOutputCapture::stdout(
            child.take_stdout().expect("piped stdout"),
            MAX_CAPTURE_BYTES,
        )
        .map_err(capture_error)?;
        let mut stderr = NativeOutputCapture::stderr(
            child.take_stderr().expect("piped stderr"),
            MAX_CAPTURE_BYTES,
        )
        .map_err(capture_error)?;
        // Payload writer: a separate thread so the timeout below still applies if the child
        // never reads its stdin. Dropping the handle closes stdin (EOF).
        let writer = child.take_stdin().map(|mut s| {
            let payload = if via_stdin {
                input.unwrap_or("").to_string()
            } else {
                String::new()
            };
            std::thread::spawn(move || {
                let _ = s.write_all(payload.as_bytes());
            })
        });
        let status = match child.wait_timeout(self.timeout) {
            Ok(Some(status)) => status,
            Ok(None) => {
                let _ = child.terminate_tree();
                return Err(Error::Timeout {
                    command: label,
                    seconds: self.timeout.as_secs(),
                });
            }
            Err(source) => {
                return Err(Error::Spawn {
                    path: self.engine.clone(),
                    source,
                })
            }
        };
        if let Some(writer) = writer {
            let _ = writer.join();
        }
        let stdout_bytes = stdout.finish_within(Duration::from_secs(2));
        let stderr_bytes = stderr.finish_within(Duration::from_secs(2));
        for capture in [&stdout, &stderr] {
            if let Some(source) = capture.take_error() {
                return Err(Error::Spawn {
                    path: self.engine.clone(),
                    source,
                });
            }
        }
        if !stdout.is_complete() || !stderr.is_complete() {
            return Err(Error::BadOutput {
                command: label,
                detail: "output pipes did not reach EOF after the engine exited".into(),
            });
        }
        if stdout_bytes.truncated || stderr_bytes.truncated {
            return Err(Error::BadOutput {
                command: label,
                detail: format!("output exceeded the {MAX_CAPTURE_BYTES}-byte capture limit"),
            });
        }
        Ok(RawOutput {
            stdout: String::from_utf8_lossy(&stdout_bytes.bytes).into_owned(),
            stderr: String::from_utf8_lossy(&stderr_bytes.bytes).into_owned(),
            code: status.code(),
        })
    }
}

/// `doctor` summary after validation.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineInfo {
    pub app_version: String,
    pub contract_version: u64,
    pub capabilities: Vec<(String, u32)>,
    pub commands: Vec<String>,
}

impl EngineInfo {
    pub fn capability(&self, name: &str) -> Option<u32> {
        self.capabilities
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
    }
}

/// One GenRight job record (`job status` / `job list` / `generate` echo).
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
    /// `{kind, width, height, duration_s}` from GenRight's probe (fields may
    /// be null); passed through untouched.
    #[serde(default)]
    pub media: Value,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

impl AssetDescriptor {
    /// True when GenRight's probe left a dimension or duration the media kind
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
    /// GenRight default 50, clamped to 1..=500.
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
pub struct GenRightClient<R: Runner> {
    runner: R,
    sink: Mutex<Option<Sink>>,
}

impl<R: Runner> GenRightClient<R> {
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
        for line in raw.stderr.lines() {
            let line = line.trim();
            if line.starts_with('{') {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    self.emit(progress_event(&v, None, Some(&label)));
                }
            }
        }
        let body = raw.stdout.trim();
        if body.is_empty() {
            return Err(Error::EngineExit {
                command: label,
                code: raw.code,
                stderr: tail(&raw.stderr),
            });
        }
        let value: Value = serde_json::from_str(body).map_err(|e| Error::BadOutput {
            command: label.clone(),
            detail: e.to_string(),
        })?;
        // A job record legitimately carries `error` ({code,message} on a
        // failed/cancelled job); the command error envelope never carries a
        // job_id. Only the envelope is a command failure.
        if let Some(err) = value
            .get("error")
            .filter(|e| e.is_object())
            .filter(|_| value.get("job_id").is_none() && value.get("status").is_none())
        {
            return Err(Error::Engine {
                code: err["code"].as_str().unwrap_or("unknown").to_string(),
                message: err["message"].as_str().unwrap_or("").to_string(),
            });
        }
        Ok(value)
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
        let v = parse_doctor(&report.to_string()).map_err(|e| Error::BadOutput {
            command: "doctor".to_string(),
            detail: e.to_string(),
        })?;
        let contract = v["contract_version"].as_u64().unwrap_or(0);
        if contract != REQUIRED_CONTRACT_VERSION {
            return Err(Error::ContractVersion {
                found: contract,
                required: REQUIRED_CONTRACT_VERSION,
            });
        }
        let capabilities: Vec<(String, u32)> = v["capabilities"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| {
                        Some((
                            c["name"].as_str()?.to_string(),
                            c["version"].as_u64()? as u32,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let info = EngineInfo {
            app_version: v["app_version"].as_str().unwrap_or("").to_string(),
            contract_version: contract,
            // GenRight-specific metadata is outside the shared doctor summary.
            commands: report["commands"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|c| c.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            capabilities,
        };
        for (name, required) in REQUIRED_CAPABILITIES {
            let found = info.capability(name);
            if found.is_none_or(|f| f < required) {
                return Err(Error::MissingCapability {
                    name: name.to_string(),
                    required,
                    found,
                });
            }
        }
        Ok(info)
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

    /// `generate <kind>` with a caller request id (GenRight maps one request
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

    /// Recent jobs (GenRight clamps `limit` to 1..=500). Prefer
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

    /// `job status --request-id <id>`. `Ok(None)` is GenRight's authoritative
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

    /// `job ack <id>`: the result is durably imported; GenRight may release it.
    pub fn job_ack(&self, job_id: &str, consumer: &str) -> Result<JobRecord> {
        self.typed(
            &["job", "ack", job_id],
            Some(&json!({"consumer": consumer})),
        )
    }

    /// `job cancel <id>` (capability `job.cancel`): asks GenRight to stop a
    /// running job; a finished job's assets are never removed.
    pub fn job_cancel(&self, job_id: &str) -> Result<JobRecord> {
        self.typed(&["job", "cancel", job_id], None)
    }
}

/// Map one GenRight NDJSON progress object to a host event.
/// `job.progress` lines become `genright.job_progress` with the typed fields
/// lifted; anything else stays a generic `genright.progress`.
pub fn progress_event(line: &Value, request_id: Option<&str>, command: Option<&str>) -> Value {
    if line["event"] == "job.progress" {
        let mut ev = json!({
            "event": "genright.job_progress",
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
    json!({"event": "genright.progress", "command": command, "data": line})
}

fn tail(s: &str) -> String {
    let s = s.trim();
    let start = s.len().saturating_sub(400);
    let start = (start..=s.len())
        .find(|i| s.is_char_boundary(*i))
        .unwrap_or(0);
    s[start..].to_string()
}
