//! Contract types: discovery files, roles, transcript types and the v1
//! `EngineFrame` envelope. Field names and values are HeardRight's v1 wire
//! format, unchanged. Everything added for protocol minor 1 (engine selection,
//! language, engine inventory, progress) is optional on the wire.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Value};

pub const SPEECH_FAMILY: &str = "parakeet-tdt-v3";
/// The service name under `<suite>/runtime/`: HeardRight's `speech-service` directory.
pub const SERVICE_NAME: &str = "speech-service";
pub const SERVICE_PROTOCOL_MAJOR: u32 = 1;
/// Minor 0: HeardRight's original host (Parakeet only, no progress frames).
pub const SERVICE_PROTOCOL_MINOR: u32 = 0;
/// Minor 1 (additive): engine inventory in capabilities, optional `engine` and
/// `language` on file requests, `engine`/`language` echoed in results, and
/// `file_transcription_progress` frames. A client sends the new fields only to a
/// host whose `service.json` says `protocol_minor >= 1`.
pub const SERVICE_PROTOCOL_MINOR_ENGINES: u32 = 1;
pub const ERROR_HOST_HANDOFF: &str = "speech_service_host_handoff";
pub const ERROR_NOT_ALLOWED: &str = "speech_service_request_not_allowed";
/// Minor 1: the requested engine is unknown, not installed, or not ready. Not retryable.
pub const ERROR_ENGINE_UNAVAILABLE: &str = "speech_service_engine_unavailable";
pub const SCHEMA_PROGRESS: &str = "file_transcription_progress";
pub const SERVICE_ALLOWED_SCHEMAS: &[&str] = &["engine_health", "engine_capabilities", "engine_info", "file_transcription_request"];
const ENGINE_PROTOCOL_MAJOR: u16 = 1;
const ENGINE_PROTOCOL_MINOR: u16 = 0;
const ENGINE_SCHEMA_VERSION: u16 = 1;
const SERVICE_ENGINE_VERSION: &str = "rightkit-speech-client";

pub const ENGINE_PARAKEET: &str = "parakeet";
pub const ENGINE_WHISPER: &str = "whisper";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostRole {
    HeardRightShell,
    HeardRightHeadless,
    ScrapeRight,
    CutRight,
}

impl HostRole {
    pub fn rank(self) -> u8 {
        match self {
            Self::HeardRightShell => 0,
            Self::HeardRightHeadless => 1,
            Self::ScrapeRight => 2,
            Self::CutRight => 3,
        }
    }
    pub fn outranks(self, other: Self) -> bool {
        self.rank() < other.rank()
    }
    /// The wire value (`service.json` `role`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HeardRightShell => "heard_right_shell",
            Self::HeardRightHeadless => "heard_right_headless",
            Self::ScrapeRight => "scrape_right",
            Self::CutRight => "cut_right",
        }
    }
    pub fn from_wire(s: &str) -> Option<Self> {
        [Self::HeardRightShell, Self::HeardRightHeadless, Self::ScrapeRight, Self::CutRight].into_iter().find(|r| r.as_str() == s)
    }
    /// `HR_SPEECH_SERVICE_ROLE` values (the engine's override spelling).
    pub fn parse_env(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "scraperight" => Some(Self::ScrapeRight),
            "cutright" => Some(Self::CutRight),
            "heardright_shell" | "heardright" => Some(Self::HeardRightShell),
            "heardright_headless" => Some(Self::HeardRightHeadless),
            _ => None,
        }
    }
    /// Role -> rank table for `rightkit_service::ServiceConfig::known_roles`.
    pub fn rank_table() -> std::collections::BTreeMap<String, u8> {
        [Self::HeardRightShell, Self::HeardRightHeadless, Self::ScrapeRight, Self::CutRight].into_iter().map(|r| (r.as_str().to_string(), r.rank())).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRecord {
    pub protocol_major: u32,
    pub protocol_minor: u32,
    pub role: HostRole,
    pub pid: u32,
    pub endpoint: String,
    pub engine_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeoverRequest {
    pub role: HostRole,
    pub pid: u32,
    pub requested_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledEngine {
    pub engine_path: PathBuf,
    pub engine_version: String,
}

pub fn service_dir(root: &Path) -> PathBuf {
    root.join("runtime").join(SERVICE_NAME)
}
pub fn service_record_path(root: &Path) -> PathBuf {
    service_dir(root).join("service.json")
}
pub fn takeover_request_path(root: &Path) -> PathBuf {
    service_dir(root).join("takeover.json")
}
pub fn installed_heardright_path(root: &Path) -> PathBuf {
    service_dir(root).join("installed").join("heardright.json")
}

/// HeardRight's endpoint naming. Windows: `\\.\pipe\orthic-labs-speech-<user_tag>-<fnv1a8(root)>`
/// (`user_tag` is the current user's SID). Unix: `<service_dir>/speech.sock`.
pub fn endpoint_for(root: &Path, user_tag: &str) -> String {
    if cfg!(windows) {
        format!(r"\\.\pipe\orthic-labs-speech-{user_tag}-{}", suite_root_tag(root))
    } else {
        service_dir(root).join("speech.sock").to_string_lossy().into_owned()
    }
}

fn suite_root_tag(root: &Path) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in root.to_string_lossy().to_lowercase().replace('\\', "/").trim_end_matches('/').bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimedWord {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTranscript {
    pub text: String,
    pub srt: String,
    pub vtt: String,
    #[serde(default)]
    pub words: Vec<TimedWord>,
}

/// Which engine should decode. On the wire a bare string: `"auto"`,
/// `"parakeet"`, `"whisper"`, or any future engine id the host reports in its
/// capabilities. Omitted on the wire = the host's current default (Parakeet on
/// a v1.0 host).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum EngineSelector {
    /// The host picks by language and readiness.
    #[default]
    Auto,
    Named(String),
}

impl EngineSelector {
    pub fn parakeet() -> Self {
        Self::Named(ENGINE_PARAKEET.into())
    }
    pub fn whisper() -> Self {
        Self::Named(ENGINE_WHISPER.into())
    }
    pub fn as_str(&self) -> &str {
        match self {
            Self::Auto => "auto",
            Self::Named(n) => n,
        }
    }
    pub fn parse(s: &str) -> Self {
        let s = s.trim().to_ascii_lowercase();
        if s.is_empty() || s == "auto" {
            Self::Auto
        } else {
            Self::Named(s)
        }
    }
}

impl fmt::Display for EngineSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl Serialize for EngineSelector {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for EngineSelector {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::parse(&String::deserialize(d)?))
    }
}

/// One engine a host can run, from the minor-1 capabilities payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineInfo {
    /// `parakeet`, `whisper`, or a future id.
    pub id: String,
    /// BCP-47-ish language codes the engine decodes; empty = unspecified/multilingual.
    #[serde(default)]
    pub languages: Vec<String>,
    /// Installed and able to decode now.
    #[serde(default)]
    pub ready: bool,
    /// Currently resident in memory (a ready engine may still need a cold load).
    #[serde(default)]
    pub loaded: bool,
    #[serde(default)]
    pub model: Option<String>,
}

/// Parsed capabilities response.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Capabilities {
    pub capabilities: Vec<String>,
    /// Engine inventory. A v1.0 host reports none; [`Capabilities::engines_or_default`] supplies Parakeet.
    pub engines: Vec<EngineInfo>,
}

impl Capabilities {
    pub fn supports_file_transcription(&self) -> bool {
        self.capabilities.iter().any(|c| c == "file_transcription")
    }
    pub fn engines_or_default(&self) -> Vec<EngineInfo> {
        if self.engines.is_empty() && self.supports_file_transcription() {
            vec![EngineInfo { id: ENGINE_PARAKEET.into(), languages: vec![], ready: true, loaded: false, model: Some(SPEECH_FAMILY.into()) }]
        } else {
            self.engines.clone()
        }
    }
    pub fn engine(&self, id: &str) -> Option<EngineInfo> {
        self.engines_or_default().into_iter().find(|e| e.id == id)
    }
    /// Ready engines able to decode `language` (all ready engines when `language` is `None`
    /// or an engine lists no languages).
    pub fn ready_for(&self, language: Option<&str>) -> Vec<EngineInfo> {
        self.engines_or_default()
            .into_iter()
            .filter(|e| e.ready && language.is_none_or(|l| e.languages.is_empty() || e.languages.iter().any(|x| x.eq_ignore_ascii_case(l))))
            .collect()
    }
}

/// Transcript plus which engine produced it (minor-1 hosts; `None` from a v1.0 host, which is Parakeet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTranscriptResult {
    pub transcript: FileTranscript,
    pub engine: Option<String>,
    pub language: Option<String>,
}

/// Minor-1 progress report from the host.
#[derive(Debug, Clone, PartialEq)]
pub struct HostProgress {
    /// 0.0..=1.0 when the host knows it.
    pub fraction: Option<f32>,
    pub state: ProgressState,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressState {
    Queued,
    Running,
    /// File work yields to dictation; the host keeps sending these so a stall timer does not fire.
    PausedForDictation,
    Other(String),
}

impl ProgressState {
    fn from_wire(s: &str) -> Self {
        match s {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "paused_for_dictation" => Self::PausedForDictation,
            o => Self::Other(o.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceErrorPayload {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(default)]
    pub diagnostics: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceEnvelope {
    pub protocol_major: u16,
    pub protocol_minor: u16,
    pub schema_name: String,
    pub schema_version: u16,
    pub engine_version: String,
    pub request_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub trace_id: String,
    #[serde(default)]
    pub payload: Option<Value>,
    #[serde(default)]
    pub error: Option<ServiceErrorPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceRequest {
    Health,
    Capabilities,
    Info,
    FileTranscription { path: String },
}

/// Minor-1 optional fields of a file request (see [`ServiceEnvelope::file_options`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileOptions {
    pub engine: Option<EngineSelector>,
    pub language: Option<String>,
}

impl ServiceEnvelope {
    fn base(schema_name: &str, request_id: &str, trace_id: &str, payload: Option<Value>, error: Option<ServiceErrorPayload>) -> Self {
        Self {
            protocol_major: ENGINE_PROTOCOL_MAJOR,
            protocol_minor: ENGINE_PROTOCOL_MINOR,
            schema_name: schema_name.into(),
            schema_version: ENGINE_SCHEMA_VERSION,
            engine_version: SERVICE_ENGINE_VERSION.into(),
            request_id: request_id.into(),
            session_id: None,
            trace_id: trace_id.into(),
            payload,
            error,
        }
    }
    pub fn health(request_id: &str, trace_id: &str) -> Self {
        Self::base("engine_health", request_id, trace_id, Some(json!({"kind":"health","status":"ok","diagnostics":null})), None)
    }
    pub fn capabilities(request_id: &str, trace_id: &str) -> Self {
        Self::base("engine_capabilities", request_id, trace_id, Some(json!({"kind":"capabilities","capabilities":["file_transcription"]})), None)
    }
    /// Minor-1 capabilities response carrying the engine inventory.
    pub fn capabilities_with_engines(request_id: &str, trace_id: &str, engines: &[EngineInfo]) -> Self {
        Self::base(
            "engine_capabilities",
            request_id,
            trace_id,
            Some(json!({"kind":"capabilities","capabilities":["file_transcription"],"engines":engines})),
            None,
        )
    }
    pub fn info(request_id: &str, trace_id: &str, cold_load_s: Option<f64>, engine_version: Option<String>) -> Self {
        Self::base(
            "engine_info",
            request_id,
            trace_id,
            Some(json!({"kind":"engine_info","mode":"speech_service","cold_load_s":cold_load_s,"engine_version":engine_version})),
            None,
        )
    }
    /// A file request exactly as a v1.0 client sends it.
    pub fn file_request(request_id: &str, trace_id: &str, path: String) -> Self {
        Self::file_request_with(request_id, trace_id, path, &FileOptions::default())
    }
    /// A file request; `engine`/`language` are included only when set, so a default
    /// request is byte-identical to the v1.0 one.
    pub fn file_request_with(request_id: &str, trace_id: &str, path: String, opts: &FileOptions) -> Self {
        let mut payload = json!({"kind":"file_transcription_request","path":path});
        if let Some(e) = &opts.engine {
            payload["engine"] = json!(e);
        }
        if let Some(l) = &opts.language {
            payload["language"] = json!(l);
        }
        Self::base("file_transcription_request", request_id, trace_id, Some(payload), None)
    }
    pub fn file_result(request_id: &str, trace_id: &str, transcript: FileTranscript) -> Self {
        Self::file_result_with(request_id, trace_id, transcript, None, None)
    }
    /// Minor-1 result echoing the engine and language actually used.
    pub fn file_result_with(request_id: &str, trace_id: &str, transcript: FileTranscript, engine: Option<&str>, language: Option<&str>) -> Self {
        let mut payload = json!({"kind":"file_transcription_result","text":transcript.text,"srt":transcript.srt,"vtt":transcript.vtt,"words":transcript.words});
        if let Some(e) = engine {
            payload["engine"] = json!(e);
        }
        if let Some(l) = language {
            payload["language"] = json!(l);
        }
        Self::base("file_transcription_result", request_id, trace_id, Some(payload), None)
    }
    /// Minor-1 progress frame for `request_id`.
    pub fn progress(request_id: &str, trace_id: &str, fraction: Option<f32>, state: &str, message: Option<&str>) -> Self {
        Self::base(
            SCHEMA_PROGRESS,
            request_id,
            trace_id,
            Some(json!({"kind":SCHEMA_PROGRESS,"fraction":fraction,"state":state,"message":message})),
            None,
        )
    }
    pub fn error(request_id: &str, trace_id: &str, code: &str, message: &str, retryable: bool) -> Self {
        Self::base(
            "engine_error",
            request_id,
            trace_id,
            None,
            Some(ServiceErrorPayload { code: code.into(), message: message.into(), retryable, diagnostics: None }),
        )
    }
    pub fn validate_envelope(&self) -> Result<(), &'static str> {
        if self.protocol_major != ENGINE_PROTOCOL_MAJOR {
            return Err("protocol major mismatch");
        }
        if self.schema_version != ENGINE_SCHEMA_VERSION {
            return Err("unsupported schema version");
        }
        Ok(())
    }
    pub fn request(&self) -> Result<ServiceRequest, &'static str> {
        if self.error.is_some() {
            return Err("normal frame carried error payload");
        }
        let kind = self.payload.as_ref().and_then(|value| value.get("kind")).and_then(Value::as_str);
        match (self.schema_name.as_str(), kind) {
            ("engine_health", Some("health")) if self.payload.as_ref().is_some_and(valid_health) => Ok(ServiceRequest::Health),
            ("engine_capabilities", Some("capabilities")) if self.payload.as_ref().is_some_and(valid_capabilities) => Ok(ServiceRequest::Capabilities),
            ("engine_info", Some("engine_info")) if self.payload.as_ref().is_some_and(valid_info) => Ok(ServiceRequest::Info),
            ("file_transcription_request", Some("file_transcription_request")) => self
                .payload
                .as_ref()
                .and_then(|v| v.get("path"))
                .and_then(Value::as_str)
                .map(|path| ServiceRequest::FileTranscription { path: path.into() })
                .ok_or("missing file path"),
            _ if is_allowed_schema(&self.schema_name) => Err("payload schema mismatch"),
            _ => Err("request not allowed"),
        }
    }
    /// The minor-1 `engine`/`language` of a file request (both `None` from a v1.0 client).
    pub fn file_options(&self) -> FileOptions {
        let p = self.payload.as_ref();
        FileOptions {
            engine: p.and_then(|v| v.get("engine")).and_then(Value::as_str).map(EngineSelector::parse),
            language: p.and_then(|v| v.get("language")).and_then(Value::as_str).map(str::to_string).filter(|s| !s.is_empty()),
        }
    }
    pub fn health_status(&self) -> Option<&str> {
        let status = self.payload.as_ref()?.get("status")?.as_str()?;
        matches!(status, "ok" | "degraded" | "unavailable").then_some(status)
    }
    pub fn health_unavailable(&self) -> bool {
        self.schema_name == "engine_health" && self.health_status() == Some("unavailable")
    }
    pub fn transcript(&self) -> Option<FileTranscript> {
        (self.schema_name == "file_transcription_result").then_some(())?;
        let value = self.payload.as_ref()?;
        (value.get("kind").and_then(Value::as_str) == Some("file_transcription_result")).then_some(())?;
        serde_json::from_value(value.clone()).ok()
    }
    /// Transcript with the minor-1 `engine`/`language` echo.
    pub fn transcript_result(&self) -> Option<FileTranscriptResult> {
        let transcript = self.transcript()?;
        let p = self.payload.as_ref()?;
        Some(FileTranscriptResult {
            transcript,
            engine: p.get("engine").and_then(Value::as_str).map(str::to_string),
            language: p.get("language").and_then(Value::as_str).map(str::to_string),
        })
    }
    pub fn capabilities_info(&self) -> Option<Capabilities> {
        (self.schema_name == "engine_capabilities").then_some(())?;
        let p = self.payload.as_ref()?;
        let capabilities = p.get("capabilities")?.as_array()?.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
        let engines = p.get("engines").and_then(|e| serde_json::from_value(e.clone()).ok()).unwrap_or_default();
        Some(Capabilities { capabilities, engines })
    }
    pub fn host_progress(&self) -> Option<HostProgress> {
        (self.schema_name == SCHEMA_PROGRESS).then_some(())?;
        let p = self.payload.as_ref()?;
        Some(HostProgress {
            fraction: p.get("fraction").and_then(Value::as_f64).map(|f| f as f32),
            state: ProgressState::from_wire(p.get("state").and_then(Value::as_str).unwrap_or("running")),
            message: p.get("message").and_then(Value::as_str).map(str::to_string),
        })
    }
}

pub fn is_allowed_schema(schema: &str) -> bool {
    SERVICE_ALLOWED_SCHEMAS.contains(&schema)
}

fn valid_health(value: &Value) -> bool {
    matches!(value.get("status").and_then(Value::as_str), Some("ok" | "degraded" | "unavailable"))
}

fn valid_capabilities(value: &Value) -> bool {
    value.get("capabilities").and_then(Value::as_array).is_some_and(|items| items.iter().all(Value::is_string))
}

fn valid_info(value: &Value) -> bool {
    value.get("mode").is_some_and(Value::is_string)
        && value.get("cold_load_s").is_none_or(|field| field.is_null() || field.is_number())
        && value.get("engine_version").is_none_or(|field| field.is_null() || field.is_string())
}
