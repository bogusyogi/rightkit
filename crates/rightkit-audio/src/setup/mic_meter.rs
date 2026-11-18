//! Microphone level meter for a setup step.
//!
//! One [`MicMeter`] owns the meter lifecycle. `begin` hands out an epoch; `start` opens the
//! microphone and runs a capture thread that reports through a host callback until the epoch is
//! invalidated (`stop`, or a later `begin` for a device change). A stale capture thread must not
//! stop its successor, and a stopped meter must not reopen the microphone.
//!
//! ```ignore
//! static METER: MicMeter = MicMeter::new(host_event_sink);
//! let epoch = METER.begin();              // on the async side
//! METER.start(epoch, config, resolve, emit)?;   // on a blocking thread
//! METER.stop();                           // step exit, hub hide, suspend, destroy
//! ```
//!
//! Events: [`MeterEvent`] values go to the host's `emit` callback, which maps them to whatever it
//! delivers (renderer events, a channel). Diagnostics go to the host's `fn(&str, &str)` sink with
//! the event names in this file.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::capture::DeviceInfo;

// The meter is a level meter on a dB scale: -80 dBFS reads 0 and -10 dBFS reads full, so ambient noise
// and speech land at different heights. The earlier linear scale put full at RMS 0.003
// (-50 dBFS) and pinned at idle. Measured RMS, both OSes:
//   Windows laptop array: ambient 0.00032 (-70 dB, level 0.14, above confirmation), speech peaks 0.005-0.34
//   Mac USB mic, fan on: idle median 0.0040 (level 0.46), idle p95 0.0195 (0.66), speech peak 0.21
// NOISE_FLOOR sits below -80 dB: anything at or under it is silence.
const NOISE_FLOOR: f32 = 0.00002;
const DB_FLOOR: f32 = -80.0;
const DB_SPAN: f32 = 70.0;

/// Meter level 0..1 for a block's RMS: a dB scale from -80 dBFS (0) to -10 dBFS (1).
pub fn level(rms: f32) -> f32 {
    if rms <= NOISE_FLOOR {
        return 0.0;
    }
    let db = 20.0 * rms.max(1e-9).log10();
    ((db - DB_FLOOR) / DB_SPAN).clamp(0.0, 1.0)
}

// Bounds the history; enough blocks to know the idle floor.
const RMS_HISTORY_CAP: usize = 4096;

/// Host settings for one meter run.
#[derive(Clone, Copy)]
pub struct MeterConfig {
    /// Capture sample rate in Hz (mono, f32).
    pub sample_rate: u32,
    /// Capture block length in milliseconds.
    pub block_ms: u32,
    /// Smoothed level above which the probe counts as a confirmed signal.
    pub confirm_level: f32,
    /// How long without a confirmed signal before one silent reopen, then `NoSignal`.
    pub probe_timeout: Duration,
}

/// Status of the meter, for the host to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterStatus {
    Recovered,
    Verified,
    Reopening,
    Unavailable,
    NoSignal,
}

impl MeterStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recovered => "recovered",
            Self::Verified => "verified",
            Self::Reopening => "reopening",
            Self::Unavailable => "unavailable",
            Self::NoSignal => "no_signal",
        }
    }
}

/// What the meter tells the host.
#[derive(Debug, Clone, PartialEq)]
pub enum MeterEvent {
    /// Smoothed level, already clamped to 0..1. A final `Level(0.0)` follows a normal release.
    Level(f32),
    Status(MeterStatus),
    /// Capture failed for good. `detail` is the failing outcome (`no_callbacks`, `stream_error`,
    /// ...) when there is one; the host words the message.
    Error { detail: Option<String> },
}

/// Why `start` did not start a meter. The host words the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeterStartError {
    /// The device list could not be read.
    EnumerateDevices,
    /// The host's resolver refused the saved device; carries the host's own text.
    DeviceUnavailable(String),
    /// The microphone would not open.
    OpenFailed,
    /// The capture thread could not be spawned.
    SpawnFailed(String),
}

type OpenResult = Result<crate::capture::CaptureSession, String>;

pub struct MicMeter {
    active: AtomicBool,
    // A device change replaces an active meter. Stale capture threads must not stop its successor.
    epoch: AtomicU64,
    // Serializes every transition of the ACTIVE/EPOCH pair (start, stop, and a
    // worker releasing its own meter). A worker's "if still mine, clear ACTIVE"
    // was a load followed by a separate store, so a meter started in between
    // could be cleared by its stale predecessor. Reads stay lock-free.
    transition: std::sync::Mutex<()>,
    event_sink: fn(&str, &str),
}

impl MicMeter {
    pub const fn new(event_sink: fn(&str, &str)) -> Self {
        Self {
            active: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            transition: std::sync::Mutex::new(()),
            event_sink,
        }
    }

    fn meter_transition(&self) -> std::sync::MutexGuard<'_, ()> {
        self.transition
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Clear ACTIVE only if `epoch` still owns the meter, atomically with respect
    /// to start and stop.
    fn release(&self, epoch: u64) -> bool {
        let _transition = self.meter_transition();
        if self.epoch.load(Ordering::SeqCst) != epoch {
            return false;
        }
        self.active.store(false, Ordering::SeqCst);
        true
    }

    fn is_current(&self, epoch: u64) -> bool {
        self.active.load(Ordering::SeqCst) && self.epoch.load(Ordering::SeqCst) == epoch
    }

    /// Claim the meter for a new run and return its epoch. A meter that was active is replaced
    /// (its capture thread ends on its own); that is reported as `onboarding_mic_meter_replaced`.
    pub fn begin(&self) -> u64 {
        let (epoch, replacing_active_meter) = {
            let _transition = self.meter_transition();
            let epoch = self
                .epoch
                .fetch_add(1, Ordering::SeqCst)
                .wrapping_add(1);
            (epoch, self.active.swap(true, Ordering::SeqCst))
        };
        if replacing_active_meter {
            (self.event_sink)("onboarding_mic_meter_replaced", &format!("epoch={epoch}"));
        }
        epoch
    }

    /// Invalidate the meter's epoch so any live capture thread tears itself down.
    /// Thread-safe and idempotent. Call it from the step-exit cleanup AND from every hide, suspend
    /// or destroy path of the window that shows the meter: the meter is a native OS capture
    /// thread, so a hidden or suspended web view does NOT stop it on its own, and without this a
    /// hidden window leaves the microphone hot. Bumping the epoch (not just clearing ACTIVE) also
    /// guarantees a stale thread from a replaced meter cannot reopen capture.
    pub fn stop(&self) {
        let _transition = self.meter_transition();
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.active.store(false, Ordering::SeqCst);
        (self.event_sink)("onboarding_mic_meter_stopping", "");
    }

    /// Open the microphone for the epoch from `begin` and start the capture thread. Blocking: run
    /// it off the async runtime. `resolve_device` receives the device list and returns the chosen
    /// device id (`None` is the default device) or the host's refusal text.
    /// Host must verify an existing OS microphone grant & explicit user action before calling.
    pub fn start(
        &'static self,
        epoch: u64,
        config: MeterConfig,
        resolve_device: impl FnOnce(&[DeviceInfo]) -> Result<Option<usize>, String>,
        emit: Arc<dyn Fn(MeterEvent) + Send + Sync>,
    ) -> Result<(), MeterStartError> {
        let (device_id, device_platform_id) = self
            .resolve_device(resolve_device)
            .inspect_err(|_| {
                self.release(epoch);
                (self.event_sink)(
                    "onboarding_mic_meter_device_unavailable",
                    "outcome=selected_device_unavailable",
                );
            })?;
        if !self.is_current(epoch) {
            return Ok(());
        }
        let transition = self.meter_transition();
        if !self.is_current(epoch) {
            return Ok(());
        }
        let opened = open_session(&config, device_id, device_platform_id.as_deref());
        drop(transition);
        let mut session = opened
            .map_err(|_| MeterStartError::OpenFailed)
            .inspect_err(|_| {
                self.release(epoch);
                (self.event_sink)("onboarding_mic_meter_open_failed", "outcome=open_failed");
            })?;
        if !self.is_current(epoch) {
            let _ = session.stop();
            return Ok(());
        }
        log_health(
            self.event_sink,
            "onboarding_mic_meter_started",
            &session,
            0,
            "opened",
        );

        std::thread::Builder::new()
            .name("onboarding-mic-meter".to_string())
            .spawn(move || {
                let event_sink = self.event_sink;
                let mut health = session.metrics_snapshot();
                let mut session = Some(session);
                let mut smoothed = 0.0_f32;
                let mut max_rms = 0.0_f32;
                let mut max_level = 0.0_f32;
                let mut level_blocks = 0_u64;
                // Per-block RMS, capped, so the summary can report the idle floor (min, median)
                // and not only the peak.
                let mut rms_history: Vec<f32> = Vec::new();
                let mut repair_attempt = 0_u8;
                let mut needs_health_probe = true;
                let mut probe_started = Instant::now();
                let mut signal_confirmed = false;
                while self.is_current(epoch) {
                    let Some(active_session) = session.as_mut() else {
                        break;
                    };
                    let next_samples = if needs_health_probe {
                        first_buffer(active_session).map_err(str::to_string)
                    } else {
                        active_session
                            .read_f32_blocking(320, 80)
                            .map_err(|_| "stream_error".to_string())
                            .and_then(|samples| {
                                if samples.is_empty() {
                                    Err("no_samples".to_string())
                                } else {
                                    Ok(samples)
                                }
                            })
                    };
                    match next_samples {
                        Ok(samples) if !samples.is_empty() => {
                            if needs_health_probe && repair_attempt == 1 {
                                log_health(
                                    event_sink,
                                    "onboarding_mic_meter_recovered",
                                    active_session,
                                    repair_attempt,
                                    "live",
                                );
                                if self.is_current(epoch) {
                                    emit(MeterEvent::Status(MeterStatus::Recovered));
                                }
                            }
                            needs_health_probe = false;
                            let rms = (samples.iter().map(|s| s * s).sum::<f32>()
                                / samples.len() as f32)
                                .sqrt();
                            let level = level(rms);
                            smoothed = smoothed.mul_add(0.5, level * 0.5);
                            max_rms = max_rms.max(rms);
                            if rms_history.len() < RMS_HISTORY_CAP {
                                rms_history.push(rms);
                            }
                            max_level = max_level.max(smoothed);
                            level_blocks = level_blocks.saturating_add(1);
                            // The read above can outlive a stop or replacement;
                            // a stale meter must not paint the new one's UI.
                            if !self.is_current(epoch) {
                                break;
                            }
                            emit(MeterEvent::Level(smoothed.clamp(0.0, 1.0)));
                            if !signal_confirmed && smoothed > config.confirm_level {
                                signal_confirmed = true;
                                emit(MeterEvent::Status(MeterStatus::Verified));
                            }
                            if !signal_confirmed && probe_started.elapsed() >= config.probe_timeout
                            {
                                if repair_attempt == 0 {
                                    event_sink(
                                        "onboarding_mic_meter_silent_reopen",
                                        "outcome=no_signal repair_attempt=0",
                                    );
                                    emit(MeterEvent::Status(MeterStatus::Reopening));
                                    if let Some(mut previous) = session.take() {
                                        health = previous.stop();
                                    }
                                    // Same epoch-checked, single-retry helper as
                                    // the stream-error path: a stopped meter
                                    // must not reopen the microphone.
                                    let Some(reopen_result) = self.try_reopen(
                                        epoch,
                                        &config,
                                        &mut repair_attempt,
                                        device_id,
                                        device_platform_id.as_deref(),
                                    ) else {
                                        break;
                                    };
                                    match reopen_result {
                                        Ok(mut reopened) => {
                                            if !self.is_current(epoch) {
                                                let _ = reopened.stop();
                                                break;
                                            }
                                            session = Some(reopened);
                                            needs_health_probe = true;
                                            probe_started = Instant::now();
                                            continue;
                                        }
                                        Err(_) => {
                                            if self.is_current(epoch) {
                                                emit(MeterEvent::Status(MeterStatus::Unavailable));
                                            }
                                            break;
                                        }
                                    }
                                }
                                emit(MeterEvent::Status(MeterStatus::NoSignal));
                                break;
                            }
                        }
                        Ok(_) => {
                            needs_health_probe = false;
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        Err(outcome) => {
                            log_health(
                                event_sink,
                                if repair_attempt == 0 {
                                    "onboarding_mic_meter_unhealthy"
                                } else {
                                    "onboarding_mic_meter_error"
                                },
                                active_session,
                                repair_attempt,
                                &outcome,
                            );
                            if !self.is_current(epoch) {
                                break;
                            }
                            emit(MeterEvent::Status(MeterStatus::Reopening));
                            if let Some(mut previous) = session.take() {
                                health = previous.stop();
                            }
                            let Some(reopen_result) = self.try_reopen(
                                epoch,
                                &config,
                                &mut repair_attempt,
                                device_id,
                                device_platform_id.as_deref(),
                            ) else {
                                // `None` also means the meter was stopped or
                                // replaced meanwhile; that is not a failure.
                                if !self.is_current(epoch) {
                                    break;
                                }
                                {
                                    emit(MeterEvent::Status(MeterStatus::Unavailable));
                                    emit(MeterEvent::Error {
                                        detail: Some(outcome.clone()),
                                    });
                                }
                                break;
                            };
                            match reopen_result {
                                Ok(mut reopened) => {
                                    if !self.is_current(epoch) {
                                        let _ = reopened.stop();
                                        break;
                                    }
                                    session = Some(reopened);
                                    needs_health_probe = true;
                                    probe_started = Instant::now();
                                }
                                Err(_) => {
                                    event_sink(
                                        "onboarding_mic_meter_error",
                                        "outcome=reopen_failed repair_attempt=1",
                                    );
                                    if self.is_current(epoch) {
                                        emit(MeterEvent::Status(MeterStatus::Unavailable));
                                        emit(MeterEvent::Error { detail: None });
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }
                if let Some(mut current) = session.take() {
                    health = current.stop();
                }
                rms_history.sort_by(f32::total_cmp);
                let min_rms = rms_history.first().copied().unwrap_or(0.0);
                let median_rms = rms_history
                    .get(rms_history.len() / 2)
                    .copied()
                    .unwrap_or(0.0);
                event_sink(
                    "onboarding_mic_meter_summary",
                    &format!(
                        "epoch={epoch} blocks={level_blocks} min_rms={min_rms:.8} median_rms={median_rms:.8} max_rms={max_rms:.8} max_level={max_level:.4} callbacks={} input_samples={} async_errors={} dropped_blocks={}",
                        health.callback_invocations,
                        health.input_samples_received,
                        health.async_error_count,
                        health.dropped_blocks,
                    ),
                );
                if self.release(epoch) {
                    emit(MeterEvent::Level(0.0));
                }
            })
            .map_err(|err| {
                self.release(epoch);
                MeterStartError::SpawnFailed(err.to_string())
            })?;

        Ok(())
    }

    fn resolve_device(
        &self,
        resolve: impl FnOnce(&[DeviceInfo]) -> Result<Option<usize>, String>,
    ) -> Result<(Option<usize>, Option<String>), MeterStartError> {
        let devices = crate::capture::list_input_devices()
            .map_err(|_| MeterStartError::EnumerateDevices)?;
        let device_id = resolve(&devices).map_err(MeterStartError::DeviceUnavailable)?;
        let platform_id = device_id.and_then(|id| {
            devices
                .iter()
                .find(|device| device.id == id)
                .and_then(|device| device.platform_id.clone())
        });
        Ok((device_id, platform_id))
    }

    fn try_reopen(
        &self,
        epoch: u64,
        config: &MeterConfig,
        repair_attempt: &mut u8,
        device_id: Option<usize>,
        device_platform_id: Option<&str>,
    ) -> Option<OpenResult> {
        let _transition = self.meter_transition();
        if !self.is_current(epoch) || *repair_attempt >= 1 {
            return None;
        }
        *repair_attempt = repair_attempt.saturating_add(1);
        Some(open_session(config, device_id, device_platform_id))
    }
}

fn open_session(
    config: &MeterConfig,
    device_id: Option<usize>,
    device_platform_id: Option<&str>,
) -> OpenResult {
    crate::capture::CaptureSession::start(crate::capture::SessionConfig {
        device_id,
        device_platform_id: device_platform_id.map(str::to_string),
        target_rate: config.sample_rate,
        target_channels: 1,
        target_dtype: crate::capture::OutDType::Float32,
        block_ms: config.block_ms,
    })
    .map_err(|e| e.to_string())
}

fn first_buffer(session: &mut crate::capture::CaptureSession) -> Result<Vec<f32>, &'static str> {
    match session.wait_first_buffer(320, 750) {
        Ok(crate::capture::CaptureFirstBuffer::Data(samples)) if !samples.is_empty() => Ok(samples),
        Ok(crate::capture::CaptureFirstBuffer::Data(_)) => Err("no_samples"),
        Ok(crate::capture::CaptureFirstBuffer::NoCallbacks) => Err("no_callbacks"),
        Ok(crate::capture::CaptureFirstBuffer::NoSamples) => Err("no_samples"),
        Ok(crate::capture::CaptureFirstBuffer::StreamError(kind)) => Err(kind.as_str()),
        Err(_) => Err("read_failed"),
    }
}

fn log_health(
    event_sink: fn(&str, &str),
    event: &str,
    session: &crate::capture::CaptureSession,
    repair_attempt: u8,
    outcome: &str,
) {
    let health = session.metrics_snapshot();
    event_sink(
        event,
        &format!(
            "outcome={outcome} repair_attempt={repair_attempt} callbacks={} input_samples={} first_callback_ms={} async_errors={} cpal_error_kind={} input_rate={} channels={} sample_format={} dropped_blocks={} overflows={} callback_max_us={} downmix={}",
            health.callback_invocations,
            health.input_samples_received,
            health
                .first_callback_latency_us
                .map(|us| (us / 1000).to_string())
                .unwrap_or_else(|| "none".to_string()),
            health.async_error_count,
            health
                .terminal_error_kind
                .or(health.last_async_error_kind)
                .map(crate::capture::CaptureErrorKind::as_str)
                .unwrap_or("none"),
            session.input_rate(),
            session.input_channels(),
            session.input_sample_format(),
            health.dropped_blocks,
            health.input_overflow_count,
            health.max_callback_duration_us,
            health
                .downmix_locked_channel
                .map(|ch| format!("locked_ch{ch}"))
                .unwrap_or_else(|| "average".to_string()),
        ),
    );
}
