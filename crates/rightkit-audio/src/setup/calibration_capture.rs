//! One-shot calibration capture for a setup step: a single-slot recorder that collects mono PCM
//! on a worker thread until the host stops it.
//!
//! PCM stays in memory until the host takes it; what the host does with it (write a file, run an
//! engine pass, remove it) is the host's. The recorder owns the parts that must not be
//! re-derived per app: one capture at a time, session ownership on stop and cancel, superseding a
//! capture left by a session that no longer exists, a hard length ceiling, and restore-once media
//! release on every exit path.
//!
//! ```no_run
//! use rightkit_audio::setup::calibration_capture::{
//!     CaptureSettings, Recorder, StartError, StartHooks, StopOutcome,
//! };
//!
//! fn record_until_stop(
//!     recorder: &Recorder,
//!     settings: &CaptureSettings,
//!     hooks: StartHooks<'_>,
//!     wait_for_stop: impl FnOnce(),
//! ) -> Result<StopOutcome, StartError> {
//!     let session_id = recorder.start(settings, hooks)?;
//!     wait_for_stop();
//!     Ok(recorder.stop(&session_id, settings, "capture_stopped"))
//! }
//! ```
//!
//! On cancellation, call `cancel` with the same session ID instead of `stop`.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

use super::media_guard::MediaPauseGuard;

/// Host settings for a calibration capture.
#[derive(Clone, Copy)]
pub struct CaptureSettings {
    pub sample_rate: u32,
    /// Shortest recording `stop` accepts.
    pub min_seconds: u32,
    /// Capacity the buffer is sized for.
    pub target_seconds: u32,
    /// Wall-clock ceiling & maximum PCM duration, including time without callbacks.
    pub max_seconds: u32,
}

/// The device the host selected for this capture.
#[derive(Clone)]
pub struct CaptureDevice {
    pub device_id: Option<usize>,
    pub platform_id: Option<String>,
}

/// The host's side of `start`, called in this order while the recorder's lock is held.
pub struct StartHooks<'a> {
    /// Remove stale output of an earlier run that crashed. Called only while no capture is active.
    pub sweep_stale: &'a dyn Fn(),
    /// A fresh session id.
    pub new_session_id: &'a dyn Fn() -> String,
    /// The selected microphone, or the host's refusal text.
    pub select_device: &'a dyn Fn() -> Result<CaptureDevice, String>,
    /// Pause other media for this session and return the guard that restores it.
    pub begin_media: &'a dyn Fn(&str) -> MediaPauseGuard,
    /// Called once the worker is running and before the capture is registered.
    pub on_recording: &'a dyn Fn(&str),
}

/// Why `start` did not start a capture. The host words the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// The host's device selection refused; carries the host's own text.
    Device(String),
    /// The capture thread could not be spawned; carries the OS error text.
    Spawn(String),
}

/// What `stop` found.
pub enum StopOutcome {
    /// The recording, at least `min_seconds` long.
    Captured(Vec<f32>),
    /// The worker recorded a failure (`capture_unavailable:..` or `capture_failed:..`).
    CaptureError(String),
    /// Shorter than `min_seconds`.
    TooShort,
    /// No capture is active.
    NotRecording,
    /// A capture is active but belongs to another session; it is left in place.
    SessionMismatch,
}

/// What `cancel` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    Cancelled,
    Idle,
    SessionMismatch,
}

struct ActiveCapture {
    setup_id: String,
    stop: Arc<AtomicBool>,
    samples: Arc<Mutex<Vec<f32>>>,
    error: Arc<Mutex<Option<String>>>,
    worker: std::thread::JoinHandle<()>,
    media: MediaPauseGuard,
}

enum TakeCapture {
    Taken(ActiveCapture),
    Idle,
    Mismatch,
}

pub struct Recorder {
    transition: Mutex<()>,
    active: Mutex<Option<ActiveCapture>>,
    event_sink: fn(&str, &str),
}

impl Recorder {
    pub const fn new(event_sink: fn(&str, &str)) -> Self {
        Self {
            transition: Mutex::new(()),
            active: Mutex::new(None),
            event_sink,
        }
    }

    /// Check the session id and take ownership under one lock. Taking first and
    /// putting a mismatched capture back later let a Start that ran in between
    /// be overwritten by the restored capture.
    fn take_active_capture(&self, session_id: &str) -> TakeCapture {
        let mut slot = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match slot.as_ref() {
            None => TakeCapture::Idle,
            Some(capture) if capture.setup_id != session_id => TakeCapture::Mismatch,
            Some(_) => slot.take().map_or(TakeCapture::Idle, TakeCapture::Taken),
        }
    }

    /// End the capture left in `slot` by a session that no longer exists, so a new Start can run:
    /// stop and join its worker (the microphone is released), restore the media it paused and drop
    /// its samples. Returns whether there was one. The caller holds the `active` lock, so no Stop
    /// or Cancel can take the capture while it is being ended.
    fn supersede_active_capture(&self, slot: &mut Option<ActiveCapture>) -> bool {
        let Some(old) = slot.take() else {
            return false;
        };
        old.stop.store(true, Ordering::Release);
        let _ = old.worker.join();
        let _ = old
            .media
            .restore_for_session(&old.setup_id, "capture_superseded");
        old.samples
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        (self.event_sink)(
            "onboarding_calibration_capture_superseded",
            &format!("session_id={}", old.setup_id),
        );
        true
    }

    /// Start a capture and return its session id. A capture still in the slot belongs to a
    /// session that was lost (the step was left while recording, the hub WebView reloaded): only
    /// one capture exists at a time, so this Start replaces it instead of failing for the rest of
    /// the process lifetime. Blocking: run it off the async runtime.
    /// Host must verify an existing OS microphone grant & explicit user action before calling.
    pub fn start(
        &self,
        settings: &CaptureSettings,
        hooks: StartHooks<'_>,
    ) -> Result<String, StartError> {
        let _transition = self
            .transition
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.supersede_active_capture(&mut active);
        (hooks.sweep_stale)();
        let setup_id = (hooks.new_session_id)();
        let sample_rate = settings.sample_rate;
        let stop = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(Mutex::new(Vec::with_capacity(
            (sample_rate * settings.target_seconds) as usize,
        )));
        let error = Arc::new(Mutex::new(None));
        let stop_worker = Arc::clone(&stop);
        let samples_worker = Arc::clone(&samples);
        let error_worker = Arc::clone(&error);
        let device = (hooks.select_device)().map_err(StartError::Device)?;
        let device_id = device.device_id;
        let media = (hooks.begin_media)(&setup_id);
        let media_worker = media.clone();
        let device_worker = device.clone();
        let event_sink = self.event_sink;
        let max_seconds = settings.max_seconds;
        let capture_started = Instant::now();
        let capture_budget = Duration::from_secs(u64::from(max_seconds));
        let worker = std::thread::Builder::new()
            .name("onboarding-recognition-capture".to_string())
            .spawn(move || {
                let mut capture =
                    match crate::capture::CaptureSession::start(crate::capture::SessionConfig {
                        device_id,
                        device_platform_id: device_worker.platform_id.clone(),
                        target_rate: sample_rate,
                        target_channels: 1,
                        target_dtype: crate::capture::OutDType::Float32,
                        block_ms: 20,
                    }) {
                        Ok(capture) => capture,
                        Err(error) => {
                            *error_worker
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                                Some(format!("capture_unavailable:{error}"));
                            media_worker.restore("capture_open_failed");
                            return;
                        }
                    };
                let max_samples = (sample_rate * max_seconds) as usize;
                let mut limit_reached = false;
                while !stop_worker.load(Ordering::Acquire) {
                    let remaining = capture_budget.saturating_sub(capture_started.elapsed());
                    let wait_ms = remaining.as_millis().min(500) as u64;
                    if wait_ms == 0 {
                        limit_reached = true;
                        break;
                    }
                    match capture.read_f32_blocking(320, wait_ms) {
                        Ok(block) => {
                            let mut samples = samples_worker
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            samples.extend(block);
                            if samples.len() >= max_samples {
                                // Keep what was captured for Stop; release the mic
                                // and media now.
                                samples.truncate(max_samples);
                                limit_reached = true;
                                break;
                            }
                        }
                        Err(error) => {
                            *error_worker
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                                Some(format!("capture_failed:{error}"));
                            let _ = capture.stop();
                            drop(capture);
                            media_worker.restore("capture_read_failed");
                            return;
                        }
                    }
                }
                let _ = capture.stop();
                drop(capture);
                if limit_reached {
                    media_worker.restore("capture_limit_reached");
                    event_sink(
                        "onboarding_calibration_capture_limit",
                        &format!("seconds={max_seconds}"),
                    );
                }
            })
            .map_err(|error| {
                media.restore("capture_thread_spawn_failed");
                StartError::Spawn(error.to_string())
            })?;
        (hooks.on_recording)(&setup_id);
        *active = Some(ActiveCapture {
            setup_id: setup_id.clone(),
            stop,
            samples,
            error,
            worker,
            media,
        });
        Ok(setup_id)
    }

    /// Idempotently discard an in-progress recording. The samples are dropped, never returned.
    pub fn cancel(&self, session_id: &str, restore_reason: &str) -> CancelOutcome {
        let _transition = self
            .transition
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let capture = match self.take_active_capture(session_id) {
            TakeCapture::Taken(capture) => capture,
            TakeCapture::Idle => return CancelOutcome::Idle,
            TakeCapture::Mismatch => return CancelOutcome::SessionMismatch,
        };
        capture.stop.store(true, Ordering::Release);
        let _ = capture.worker.join();
        let _ = capture
            .media
            .restore_for_session(session_id, restore_reason);
        capture
            .samples
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        CancelOutcome::Cancelled
    }

    /// End the capture for `session_id`, release the microphone and the media, and hand back what
    /// was recorded. Blocking (joins the worker): run it off the async runtime.
    pub fn stop(
        &self,
        session_id: &str,
        settings: &CaptureSettings,
        restore_reason: &str,
    ) -> StopOutcome {
        let _transition = self
            .transition
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let capture = match self.take_active_capture(session_id) {
            TakeCapture::Taken(capture) => capture,
            TakeCapture::Idle => return StopOutcome::NotRecording,
            TakeCapture::Mismatch => return StopOutcome::SessionMismatch,
        };
        capture.stop.store(true, Ordering::Release);
        let _ = capture.worker.join();
        let _ = capture
            .media
            .restore_for_session(session_id, restore_reason);
        if let Some(error) = capture
            .error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            return StopOutcome::CaptureError(error);
        }
        let samples = std::mem::take(
            &mut *capture
                .samples
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        if samples.len() < (settings.sample_rate * settings.min_seconds) as usize {
            return StopOutcome::TooShort;
        }
        StopOutcome::Captured(samples)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let slot = self
            .active
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(capture) = slot.take() {
            capture.stop.store(true, Ordering::Release);
            let _ = capture.worker.join();
            capture.media.restore("capture_guard_drop");
        }
    }
}

/// Little-endian f32 bytes of `samples`, the layout of a raw PCM file.
pub fn f32le_bytes(samples: &[f32]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect::<Vec<_>>()
}

/// Remove files with `extension` left in `dir` by a run that crashed or failed mid-write. A
/// missing directory is a no-op.
pub fn sweep_files_with_extension(dir: &std::path::Path, extension: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == extension) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Resolve the capture device from the device list. `choose` returns the chosen device id
/// (`None` is the default device) or the host's refusal text; the device list is read as the
/// original did, an unreadable list counting as empty. The platform id is kept only when a device
/// id was chosen.
pub fn selected_capture_device(
    choose: impl FnOnce(&[crate::capture::DeviceInfo]) -> Result<Option<usize>, String>,
) -> Result<CaptureDevice, String> {
    let devices = crate::capture::list_input_devices().unwrap_or_default();
    let device_id = choose(&devices)?;
    let selected = device_id
        .and_then(|id| devices.iter().find(|device| device.id == id))
        .or_else(|| devices.iter().find(|device| device.is_default));
    Ok(CaptureDevice {
        device_id,
        platform_id: selected.and_then(|device| device_id.and_then(|_| device.platform_id.clone())),
    })
}
