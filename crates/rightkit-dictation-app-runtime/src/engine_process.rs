// Engine contract types shared by the shell supervisor and QA command dispatch.
// The resident `rightkit-dictation-engine` sidecar is the production runtime; this module
// carries only UI-facing events/info/wake params and model-dir path helpers.

use crate::delivery::DeliveryRecord;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Async push event types emitted by the engine outside of the
/// command-response cycle. Distinct channel from `EngineFrame` so the
/// command-response code path stays simple.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
// An engine event is built once and moved to one consumer; boxing it would only add an allocation
// per event.
#[allow(clippy::large_enum_variant)]
pub enum EngineEvent {
    WakeFired {
        ts: f64,
        #[serde(default)]
        fire_count: Option<u64>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        threshold: Option<f32>,
        /// `softmax[1]` of the firing window. `None` until the
        /// Phase F wake scorer is wired (`rightkit_dictation_core::wake`).
        #[serde(default)]
        score: Option<f32>,
    },
    DeliveryCompleted {
        session_id: String,
        record: DeliveryRecord,
        #[serde(default)]
        send_enter: bool,
        /// Engine stage timeline for the `utterance_timeline` telemetry event.
        #[serde(default)]
        timeline: Option<rightkit_dictation_core::engine::EngineTimeline>,
    },
    TranscriptFinal {
        session_id: String,
        text: String,
        send_enter: bool,
        raw_text: Option<String>,
        recording_ms: Option<u64>,
        /// Engine-recognized cancel-to-history: store, never deliver.
        cancelled: bool,
        #[serde(default)]
        context_request_attempted: Option<bool>,
        #[serde(default)]
        context_original_binding: Option<bool>,
        #[serde(default)]
        capture_warning: Option<String>,
        /// Engine stage timeline for the `utterance_timeline` telemetry event.
        #[serde(default)]
        timeline: Option<rightkit_dictation_core::engine::EngineTimeline>,
    },
    TranscriptPartial {
        session_id: String,
        text: String,
        revision: u64,
        audio_secs: Option<f32>,
    },
    SessionReset {
        session_id: String,
        command_dispatched: Option<String>,
        command_detail: Option<String>,
        /// Set when the session ended on a recognized command that FAILED to
        /// dispatch (e.g. missing Accessibility grant) — the shell surfaces it
        /// on the pill instead of letting failure look like success.
        command_failed: Option<String>,
    },
    /// The sidecar recognized a hands-free stop command and is about to run the
    /// final transcribe. Leave `Recording` and show "processing" NOW, before the
    /// (~1s) decode — the transcript arrives later via `TranscriptFinal`.
    TranscribingStarted {
        session_id: String,
        /// Destination the engine froze at the hands-free stop moment.
        #[serde(default)]
        stop_target: Option<rightkit_transcript_library::delivery::TargetSnapshot>,
    },
    EngineError {
        session_id: String,
        message: String,
    },
    /// Smoothed live mic amplitude while recording. The hub and onboarding use
    /// this for capture feedback; transcript delivery remains final-only.
    RecordingLevel {
        session_id: String,
        level: f32,
    },
}

/// Engine info surfaced to the renderer via the `get_engine_info` Tauri command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineInfoResult {
    pub mode: String,
    #[serde(default)]
    pub cold_load_s: Option<f64>,
    #[serde(default)]
    pub engine_version: Option<String>,
}

/// Resolve the application asset root that contains `heardright/models`.
///
/// Dev keeps models under `<repo>/app/heardright/models`; packaged/downloaded
/// builds resolve through `model_fetch::models_base` from this hint. Unlike the retired sidecar resolver, this helper never
/// requires Python or the engine executable to exist.
pub fn resolve_app_path() -> PathBuf {
    for root in candidate_runtime_roots() {
        if let Some(app_path) = first_existing(&[
            root.join("app")
                .join(crate::host::engine_host().app_resource_dir),
            root.join("resources")
                .join("app")
                .join(crate::host::engine_host().app_resource_dir),
        ])
        .and_then(|path| path.parent().map(Path::to_path_buf))
        {
            return app_path;
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("app")
}

#[derive(Debug)]
pub enum EngineProcessError {
    Resolve(String),
    Io(std::io::Error),
    Json(serde_json::Error),
    Contract(String),
    Engine(String),
    MissingStdIo,
    UnexpectedFrame(String),
    Exited(Option<i32>),
}

impl std::fmt::Display for EngineProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolve(message) => write!(f, "engine resolve failed: {message}"),
            Self::Io(err) => write!(f, "engine io failed: {err}"),
            Self::Json(err) => write!(f, "engine json failed: {err}"),
            Self::Contract(err) => write!(f, "engine contract failed: {err}"),
            Self::Engine(err) => write!(f, "engine returned error: {err}"),
            Self::MissingStdIo => write!(f, "engine child missing stdio pipe"),
            Self::UnexpectedFrame(frame) => write!(f, "unexpected engine frame: {frame}"),
            Self::Exited(code) => write!(f, "engine process exited with code {code:?}"),
        }
    }
}

impl std::error::Error for EngineProcessError {}

impl From<std::io::Error> for EngineProcessError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for EngineProcessError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// Parameters for `wake.listen_start`. All fields optional; engine fills in
/// production defaults (openWakeWord ONNX model, threshold=0.40).
#[derive(Debug, Clone, Default, Serialize)]
pub struct WakeListenParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debounce_hits: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debounce_window: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<i32>,
}

fn first_existing(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|path| path.exists()).cloned()
}

fn candidate_runtime_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        push_ancestors(&mut roots, &cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            push_ancestors(&mut roots, parent);
            push_unique(&mut roots, parent.join("resources"));
        }
    }
    roots
}

fn push_ancestors(roots: &mut Vec<PathBuf>, path: &Path) {
    for ancestor in path.ancestors() {
        push_unique(roots, ancestor.to_path_buf());
    }
}

fn push_unique(roots: &mut Vec<PathBuf>, path: PathBuf) {
    if !roots.iter().any(|existing| existing == &path) {
        roots.push(path);
    }
}
