//! The host's handle on the local voice engine: `rightkit_tts::QtsClient` pointed at the host's
//! model, runtime and voice-library locations.
//!
//! RightKit supervises the warm QTS worker process (one worker, one request at a
//! time, cancel kills it, idle shutdown, one engine per mode, backend and vocoder labels,
//! silence trimming, error codes). This module only decides *where things are*:
//!
//! - Base (0.6B): [`models::model_dir`], the store's active revision else the pre-store flat
//!   folder, so existing installs load in place with no re-download.
//! - CustomVoice / VoiceDesign (1.7B): [`models::model_dir_for_mode`].
//! - ONNX Runtime: [`models::resolved_onnxruntime`] (the host candidate order).
//! - Voice library: `<data>/voices/local/`.
//!
//! The functions below keep the signatures the studio and providers already use. The locations
//! can change at run time (an install completes), so the client is rebuilt when they do; the
//! worker is respawned by RightKit because its configuration differs.
use super::models;
pub use rightkit_tts::client::{
    cancel_active, language_id, shutdown, worker_pid, QtsError, SynthOptions, SynthResult,
};
pub use rightkit_tts::{Mode, ModeLocation, QtsClient, PRESET_SPEAKERS};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Idle window the host asks for. `<PREFIX>_QTS_IDLE_SECS` (a whole number) wins; otherwise
/// RightKit's own environment/default (600 s) applies.
fn idle_secs() -> Option<u64> {
    crate::host::env_var("QTS_IDLE_SECS").and_then(|v| v.trim().parse().ok())
}

/// Everything the client is configured from. Equal keys reuse the client.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    data_root: PathBuf,
    base_dir: PathBuf,
    custom_voice_dir: PathBuf,
    voice_design_dir: PathBuf,
    ort_runtime: Option<PathBuf>,
    idle_secs: Option<u64>,
}

impl Key {
    fn current() -> Self {
        let data_root = models::data_root();
        Self {
            // RightKit requires an absolute data root; the same folder, made absolute.
            data_root: std::path::absolute(&data_root).unwrap_or(data_root),
            base_dir: models::model_dir(),
            custom_voice_dir: models::model_dir_for_mode(Mode::CustomVoice),
            voice_design_dir: models::model_dir_for_mode(Mode::VoiceDesign),
            ort_runtime: worker_ort_runtime(),
            idle_secs: idle_secs(),
        }
    }

    fn build(&self) -> QtsClient {
        let mut client = QtsClient::new(self.idle_secs.unwrap_or(600))
            .with_data_root(self.data_root.clone())
            .with_model_dir(self.base_dir.clone())
            .with_mode_model_dir(Mode::CustomVoice, self.custom_voice_dir.clone())
            .with_mode_model_dir(Mode::VoiceDesign, self.voice_design_dir.clone());
        if let Some(runtime) = &self.ort_runtime {
            client = client.with_ort_runtime(runtime.clone());
        }
        if let Some(secs) = self.idle_secs {
            client = client.with_idle_secs(secs);
        }
        client
    }
}

/// The ONNX Runtime the worker is told to load. On macOS an unresolved library is replaced by the
/// expected bundled path so the launch fails with `runtime_missing` instead of RightKit's own
/// discovery reaching into app data (a library there cannot pass library validation).
fn worker_ort_runtime() -> Option<PathBuf> {
    let resolved = models::resolved_onnxruntime();
    #[cfg(target_os = "macos")]
    let resolved = resolved.or_else(models::bundled_onnxruntime_path);
    resolved
}

static CLIENT: Mutex<Option<(Key, &'static QtsClient)>> = Mutex::new(None);

/// The client for the current locations. A superseded client is leaked rather than dropped:
/// `QtsClient` shuts the shared worker down on drop, which could kill a worker another thread
/// has just started from the new configuration. RightKit already replaces the worker when the
/// configuration differs, and the leak is one small struct per location change.
fn client() -> &'static QtsClient {
    let key = Key::current();
    let mut slot = CLIENT.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((held, client)) = slot.as_ref() {
        if *held == key {
            return client;
        }
    }
    let built: &'static QtsClient = Box::leak(Box::new(key.build()));
    *slot = Some((key, built));
    built
}

/// The worker executable a launch would use (`RIGHTKIT_QTS_WORKER` or the host override, next to the app, or
/// `<data>/runtime/qts`; RightKit's resolver).
pub fn worker_exe() -> Result<PathBuf, QtsError> {
    client().worker_exe()
}

/// Backend/vocoder labels reported by the live worker (loads it if needed).
pub fn engine_info(backend: Option<&str>) -> Result<(String, String), QtsError> {
    client().engine_info(backend)
}

/// Synthesize `text` (any length, chunked). Clears the cancel flag on entry.
///
/// The client caps interior model silence at 450 ms unless the caller chose otherwise. Natural
/// sentence pauses are 0.3-0.5 s; the model occasionally emits a second or more of dead air
/// mid-chunk, heard as a dropout.
pub fn synthesize(text: &str, opts: &SynthOptions) -> Result<SynthResult, QtsError> {
    let mut opts = opts.clone();
    if opts.max_interior_silence_ms.is_none() {
        opts.max_interior_silence_ms = Some(MAX_INTERIOR_SILENCE_MS);
    }
    client().synthesize(text, &opts)
}

/// Longest pause kept inside one synthesized chunk.
pub const MAX_INTERIOR_SILENCE_MS: usize = 450;

/// Instant x-vector cloning: encode a short reference WAV (3 to 30 s of clean
/// speech works best) into a voice-clone prompt file at `out`. Returns `out`.
pub fn clone_from_wav(wav_path: &Path, out: &Path) -> Result<PathBuf, QtsError> {
    client().clone_from_wav(wav_path, out)
}

/// Clone into the voice library folder: writes `<data>/voices/local/<id>.xvector.pb`.
pub fn clone_into_library(wav_path: &Path, voice_id: &str) -> Result<PathBuf, QtsError> {
    client().clone_into_library(wav_path, voice_id)
}

/// A voice record path is acceptable only inside the voices folder (the provider param must not
/// read arbitrary files). Returns the canonical path.
pub fn resolve_voice_record(path: &str) -> Result<PathBuf, QtsError> {
    client().resolve_voice_record(path)
}
