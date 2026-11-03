#![allow(unused_imports)]
#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{
    probe_decisions::*, transcript_text::*, unified_probe_lane::*, worker_api::*, worker_clock::*,
    worker_main::*,
};

pub(crate) use crate::windows_audio_lifecycle::{LifecycleCause, LifecycleEvent};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CaptureRouteSignature {
    pub(crate) device_id: usize,
    pub(crate) platform_id: Option<String>,
    pub(crate) uses_system_default: bool,
    pub(crate) name: String,
    pub(crate) native_rate: u32,
    pub(crate) channels: u16,
    pub(crate) sample_format: String,
    pub(crate) transport: CaptureTransport,
    pub(crate) form_factor: rightkit_audio::capture::CaptureFormFactor,
}

pub(crate) fn resolve_capture_route(
    saved_device: Option<&str>,
    devices: &[DeviceInfo],
) -> Result<(Option<usize>, Option<CaptureRouteSignature>), String> {
    let snapshots: Vec<MicDeviceSnapshot> = devices
        .iter()
        .map(|d| MicDeviceSnapshot {
            id: d.id,
            name: d.name.clone(),
            is_default: d.is_default,
            platform_id: d.platform_id.clone(),
        })
        .collect();
    let selection = resolve_mic_selection(saved_device, &snapshots);
    if selection.fallback {
        return Err(format!(
            "saved microphone is unavailable: {}",
            selection.label
        ));
    }
    let selected = selection
        .device_id
        .and_then(|id| devices.iter().find(|device| device.id == id))
        .or_else(|| devices.iter().find(|device| device.is_default));
    let signature = selected.map(|device| CaptureRouteSignature {
        device_id: device.id,
        platform_id: device.platform_id.clone(),
        uses_system_default: selection.device_id.is_none(),
        name: device.name.clone(),
        native_rate: device.native_rate,
        channels: device.channels,
        sample_format: device.sample_format.clone(),
        transport: device.transport,
        form_factor: device.form_factor,
    });
    Ok((selection.device_id, signature))
}

pub(crate) fn current_capture_route(
    saved_device: Option<&str>,
) -> Result<Option<CaptureRouteSignature>, String> {
    let devices = list_input_devices().map_err(|e| format!("capture route query: {e}"))?;
    Ok(resolve_capture_route(saved_device, &devices)?.1)
}

pub(crate) fn capture_route_changed(
    previous: &Option<CaptureRouteSignature>,
    current: &Option<CaptureRouteSignature>,
) -> bool {
    current.is_some() && current != previous
}

/// Open a capture session honoring the saved mic selection, leaving playback
/// paused. Startup and cold recording prep use this to avoid play/pause/play.
/// The returned route signature lets later starts reuse this stream unless the
/// device, rate, channel count, or sample format changed while it was paused.
pub(crate) fn open_capture_paused(
    saved_device: Option<&str>,
) -> Result<(WorkerCapture, Option<CaptureRouteSignature>), String> {
    let devices = list_input_devices().map_err(|e| format!("capture devices: {e}"))?;
    let (device_id, route) = resolve_capture_route(saved_device, &devices)?;
    let cfg = SessionConfig {
        device_id,
        device_platform_id: route
            .as_ref()
            .filter(|signature| !signature.uses_system_default)
            .and_then(|signature| signature.platform_id.clone()),
        target_rate: SAMPLE_RATE,
        target_channels: 1,
        target_dtype: OutDType::Float32,
        block_ms: 20,
    };
    let capture =
        CaptureSession::open_paused(cfg).map_err(|error| capture_open_error(error.to_string()))?;
    Ok((WorkerCapture::Live(capture), route))
}

/// Open a capture session with playback started, preserving existing recovery
/// and wake-listen callers that need audio immediately.
pub(crate) fn open_capture(
    saved_device: Option<&str>,
) -> Result<(WorkerCapture, Option<CaptureRouteSignature>), String> {
    let (capture, route) = open_capture_paused(saved_device)?;
    capture.resume().map_err(capture_open_error)?;
    Ok((capture, route))
}

pub(crate) fn capture_open_error(detail: String) -> String {
    // WASAPI surfaces package/desktop mic denial as Access Denied. Keep the
    // OS text and add a stable hint so field logs distinguish consent from
    // a missing/broken device.
    if detail.contains("Access is denied")
        || detail.contains("0x80070005")
        || detail.contains("-2147024891")
    {
        format!(
            "capture open: {detail} (the app's microphone permission may still be denied in Windows Settings)"
        )
    } else {
        format!("capture open: {detail}")
    }
}

// --- Warm-capture liveness ---
// A warm CoreAudio stream can sit paused after relaunch, then resume and
// deliver samples that Silero scores as pure silence: the device is fine but
// the stream is dead, and the route-signature guard above finds nothing
// changed because Silero answers "is this audio speech?", not "is this audio
// alive?".
//
// Samples prove callbacks & stream delivery, not microphone usefulness.
// Digital-zero PCM remains valid user silence (including noise-gated headset
// output); non-finite PCM is invalid. Onboarding owns no-audio guidance &
// device selection.
pub(crate) const CAPTURE_LIVENESS_MIN_SAMPLES: usize = SAMPLE_RATE as usize / 40;

pub(crate) const CAPTURE_ZERO_GRACE_SAMPLES: usize = SAMPLE_RATE as usize * 3 / 2;
pub(crate) const CAPTURE_NO_AUDIO_MESSAGE: &str =
    "No microphone audio detected after reopening. Check your microphone or mute setting, then try again.";

/// A short zero-filled seed is legal (Bluetooth startup/muting). Sustained
/// exact zeros are a capture health signal, not a speech/VAD decision. Quiet
/// finite audio never trips this guard. One reopen is allowed per recording.
#[derive(Default)]
pub(crate) struct CaptureZeroGuard {
    pub(crate) trailing_zeros: usize,
    pub(crate) repair_attempted: bool,
}

impl CaptureZeroGuard {
    pub(crate) fn observe(&mut self, samples: &[f32]) {
        if let Some(last_nonzero) = samples.iter().rposition(|sample| *sample != 0.0) {
            self.trailing_zeros = samples.len() - last_nonzero - 1;
        } else {
            self.trailing_zeros = self.trailing_zeros.saturating_add(samples.len());
        }
    }

    pub(crate) fn needs_recovery(&self) -> bool {
        self.trailing_zeros >= CAPTURE_ZERO_GRACE_SAMPLES
    }

    pub(crate) fn begin_repair(&mut self) -> Result<(), &'static str> {
        if self.repair_attempted {
            return Err(CAPTURE_NO_AUDIO_MESSAGE);
        }
        self.repair_attempted = true;
        self.trailing_zeros = 0;
        Ok(())
    }
}

/// Replace only capture ownership. Recording PCM, VAD, command timestamps &
/// scheduled ASR offsets remain intact. Return every seed sample to the caller.
pub(crate) fn reopen_zero_capture(
    capture: &mut Option<WorkerCapture>,
    route: &mut Option<CaptureRouteSignature>,
    saved_device: Option<&str>,
    decision_clock: &dyn worker_clock::WorkerClock,
) -> Result<Vec<f32>, String> {
    reopen_zero_capture_with(capture, route, decision_clock, || {
        open_capture(saved_device)
    })
}

pub(crate) fn reopen_zero_capture_with(
    capture: &mut Option<WorkerCapture>,
    route: &mut Option<CaptureRouteSignature>,
    decision_clock: &dyn worker_clock::WorkerClock,
    open: impl FnOnce() -> Result<(WorkerCapture, Option<CaptureRouteSignature>), String>,
) -> Result<Vec<f32>, String> {
    if let Some(cap) = capture.as_ref() {
        cap.pause();
    }
    *capture = None;
    let (mut reopened, new_route) = open()?;
    // open_capture already starts playback; do not flush away fresh speech.
    let first = wait_for_first_capture_buffer(
        &mut reopened,
        capture_settle_budget_ms(new_route.as_ref(), true),
        decision_clock,
    )?;
    if capture_first_buffer_requires_reopen(&first) {
        reopened.pause();
        return Err(format!(
            "{CAPTURE_NO_AUDIO_MESSAGE} ({})",
            capture_first_buffer_outcome(&first)
        ));
    }
    let CaptureFirstBuffer::Data(seed) = first else {
        unreachable!("readiness check accepted only data");
    };
    *route = new_route;
    *capture = Some(reopened);
    Ok(seed)
}

/// Pure classification of the Stop-time "no microphone audio" short-circuit:
/// a live (non-replay) capture whose whole buffer is digital zero is
/// reported as an explicit error instead of being handed to ASR, which
/// would otherwise produce a garbage transcription. This is a function of
/// the samples alone and applies on every platform; a replayed capture
/// (fixture/bakeoff) is exempt so recorded zero-silence fixtures still
/// transcribe normally.
pub(crate) fn is_no_microphone_audio_at_stop(
    buffer: &[f32],
    capture_present_and_live: bool,
) -> bool {
    capture_present_and_live && !buffer.is_empty() && buffer.iter().all(|sample| *sample == 0.0)
}

pub(crate) fn capture_first_buffer_requires_reopen(first: &CaptureFirstBuffer) -> bool {
    match first {
        CaptureFirstBuffer::Data(samples) => {
            samples.is_empty() || samples.iter().any(|sample| !sample.is_finite())
        }
        CaptureFirstBuffer::NoCallbacks
        | CaptureFirstBuffer::NoSamples
        | CaptureFirstBuffer::StreamError(_) => true,
    }
}

pub(crate) fn capture_first_buffer_outcome(first: &CaptureFirstBuffer) -> &'static str {
    match first {
        CaptureFirstBuffer::Data(samples) if samples.is_empty() => "capture_stream_empty_data",
        CaptureFirstBuffer::Data(samples) => {
            if samples.iter().any(|sample| !sample.is_finite()) {
                "capture_stream_non_finite"
            } else if samples.iter().all(|sample| *sample == 0.0) {
                "capture_stream_digital_zero"
            } else {
                "live"
            }
        }
        CaptureFirstBuffer::NoCallbacks => "capture_stream_no_callbacks",
        CaptureFirstBuffer::NoSamples => "capture_stream_no_samples",
        CaptureFirstBuffer::StreamError(kind) => kind.as_str(),
    }
}

pub(crate) fn capture_settle_budget_ms(
    route: Option<&CaptureRouteSignature>,
    repair_attempt: bool,
) -> u64 {
    if route.is_some_and(|route| route.transport == CaptureTransport::Bluetooth) {
        1_500
    } else if repair_attempt {
        500
    } else {
        250
    }
}

/// Wait for callbacks to deliver a minimally useful seed. Silence is valid
/// audio; non-finite samples are rejected before VAD/ASR observes them.
pub(crate) fn wait_for_first_capture_buffer(
    capture: &mut WorkerCapture,
    timeout_ms: u64,
    decision_clock: &dyn worker_clock::WorkerClock,
) -> Result<CaptureFirstBuffer, String> {
    let timeout = Duration::from_millis(timeout_ms);
    let deadline = decision_clock.now().saturating_add(timeout);
    // Blocking capture waits consume this budget even when a test source has
    // no later event with which to advance virtual time.
    let mut wait_budget = timeout;
    let mut samples = Vec::new();
    let mut last_empty = CaptureFirstBuffer::NoCallbacks;
    loop {
        let remaining = deadline
            .duration_since(decision_clock.now())
            .min(wait_budget);
        if remaining.is_zero() {
            break;
        }
        let slice_ms = remaining.as_millis().clamp(1, 100) as u64;
        wait_budget = wait_budget.saturating_sub(Duration::from_millis(slice_ms));
        match capture
            .wait_first_buffer(CAPTURE_LIVENESS_MIN_SAMPLES, slice_ms)
            .map_err(|error| error.to_string())?
        {
            CaptureFirstBuffer::Data(mut chunk) => {
                samples.append(&mut chunk);
                if samples.len() >= CAPTURE_LIVENESS_MIN_SAMPLES {
                    return Ok(CaptureFirstBuffer::Data(samples));
                }
            }
            CaptureFirstBuffer::StreamError(kind) => {
                return Ok(CaptureFirstBuffer::StreamError(kind));
            }
            empty @ (CaptureFirstBuffer::NoCallbacks | CaptureFirstBuffer::NoSamples) => {
                last_empty = empty;
            }
        }
    }
    if samples.is_empty() {
        Ok(last_empty)
    } else {
        Ok(CaptureFirstBuffer::Data(samples))
    }
}

#[derive(Debug)]
pub(crate) struct CapturePcmHistory {
    pub(crate) block: Vec<f32>,
    pub(crate) consecutive_repeats: usize,
}

#[derive(Debug)]
pub(crate) enum CapturePcmValidation {
    Accepted(CapturePcmHistory),
    DiscardedRepeat(CapturePcmHistory),
}

impl CapturePcmValidation {
    pub(crate) fn history(self) -> CapturePcmHistory {
        match self {
            Self::Accepted(history) | Self::DiscardedRepeat(history) => history,
        }
    }

    pub(crate) fn discarded_repeat(&self) -> bool {
        matches!(self, Self::DiscardedRepeat(_))
    }
}

/// Capture is trusted only after one shared ingress check. NaN/Inf would
/// poison VAD comparisons, levels, KWS timestamps & final decode differently;
/// reject it once before any subsystem observes the block. Some Windows
/// drivers replay one callback during capture start, so discard one repeated
/// block; a second consecutive replay still proves capture is stuck.
pub(crate) fn validate_capture_pcm(
    samples: &[f32],
    previous: Option<&CapturePcmHistory>,
) -> Result<CapturePcmValidation, &'static str> {
    if !samples.iter().all(|sample| sample.is_finite()) {
        Err("capture PCM contains non-finite sample")
    } else {
        let has_variation = samples.windows(2).any(|pair| pair[0] != pair[1]);
        let consecutive_repeats = previous
            .filter(|prior| has_variation && prior.block == samples)
            .map_or(0, |prior| prior.consecutive_repeats.saturating_add(1));
        let history = CapturePcmHistory {
            block: samples.to_vec(),
            consecutive_repeats,
        };
        if consecutive_repeats >= 2 {
            Err("capture PCM repeated consecutive blocks")
        } else if consecutive_repeats == 1 {
            Ok(CapturePcmValidation::DiscardedRepeat(history))
        } else {
            Ok(CapturePcmValidation::Accepted(history))
        }
    }
}

pub(crate) fn capture_should_reopen(capture_missing: bool, saved_device_changed: bool) -> bool {
    capture_missing || saved_device_changed
}

// --- Windows endpoint & power lifecycle recovery (Wave W1) ---
// docs/WINDOWS_IMPLEMENTATION_SET.md "Wave W1 -- endpoint & power recovery".
// The adapter in `windows_audio_lifecycle.rs` only enqueues typed events; all
// coalescing, the recovery generation, and the diagnostics record live here
// so they can share `capture`/`last_capture_route` state with the existing
// capture owner instead of duplicating it.

/// Picks one representative cause for an entire coalesced burst. Power
/// transitions are ordered by the shared callback sequence: the last power
/// state wins. Endpoint-only bursts keep severity ordering.
pub(crate) fn windows_audio_lifecycle_primary_cause(
    burst: &[LifecycleEvent],
) -> Option<LifecycleCause> {
    let terminal_power = burst
        .iter()
        .filter(|event| {
            matches!(
                event.cause,
                LifecycleCause::Suspend | LifecycleCause::Resume
            )
        })
        .max_by_key(|event| event.sequence)
        .map(|event| event.cause);
    terminal_power.or_else(|| burst.iter().map(|event| event.cause).max())
}

/// Filters endpoint notifications before capture teardown. Explicit mic
/// selection ignores default-route changes; device-specific changes only
/// matter for current endpoint, or for available capture endpoints while no
/// stream exists. This also excludes unrelated render-device events without
/// doing COM work on callback threads.
pub(crate) fn windows_audio_lifecycle_event_relevant(
    event: &LifecycleEvent,
    saved_device: Option<&str>,
    capture_present: bool,
    current_route: Option<&CaptureRouteSignature>,
    available_capture_endpoint_ids: &[String],
    suspended: bool,
) -> bool {
    match event.cause {
        LifecycleCause::Suspend | LifecycleCause::Resume => true,
        _ if suspended => false,
        LifecycleCause::DefaultChanged => saved_device.is_none(),
        LifecycleCause::DeviceStateChanged
        | LifecycleCause::PropertyChanged
        | LifecycleCause::DeviceRemoved => {
            let Some(endpoint_id) = event.endpoint_id.as_deref() else {
                return true;
            };
            let is_current = current_route
                .and_then(|route| route.platform_id.as_deref())
                .is_some_and(|current| current.eq_ignore_ascii_case(endpoint_id));
            if is_current {
                return true;
            }
            !capture_present
                && available_capture_endpoint_ids
                    .iter()
                    .any(|capture_id| capture_id.eq_ignore_ascii_case(endpoint_id))
        }
    }
}

#[derive(Debug)]
pub(crate) struct WindowsLifecycleRecoveryOutcome {
    pub(crate) retry_count: u32,
    pub(crate) first_buffer_ms: Option<u64>,
    pub(crate) fallback_reason: Option<String>,
    pub(crate) outcome: &'static str,
    pub(crate) route: Option<CaptureRouteSignature>,
    // The liveness-check seed consumed off the reopened stream by
    // `wait_for_first_capture_buffer`. On a healthy `recording_resumed`
    // outcome this is real post-resume audio that a recording in flight must
    // not lose (field incident class: first buffer after resume silently
    // discarded instead of observed by VAD/appended to `buffer`, see
    // `worker/worker_main.rs`'s consumer). Empty on every other outcome.
    pub(crate) seed_samples: Vec<f32>,
}

/// Runs exactly one recovery generation: quiesce the current stream, drop
/// it, re-enumerate, resolve the saved/default route, reopen once, retry a
/// bounded number of times, then land on `suspended`/`recording_resumed`/
/// `idle_prepared`, `saved_device_unavailable`, `permission_blocked`, or
/// `failed`. Never called concurrently: the worker is single-threaded, so at
/// most one generation executes at a time and every event queued during it
/// is coalesced into the next one instead of starting a parallel recovery.
pub(crate) fn run_windows_audio_lifecycle_recovery(
    saved_device: Option<&str>,
    capture: &mut Option<WorkerCapture>,
    last_capture_route: &mut Option<CaptureRouteSignature>,
    recording: bool,
    wake_listening: bool,
    suspend_only: bool,
    decision_clock: &dyn worker_clock::WorkerClock,
) -> WindowsLifecycleRecoveryOutcome {
    // 1. Quiesce & drop. Resume must never trust a pre-event IMMDevice or
    // CPAL stream, and suspend must not let a stale stream survive sleep.
    if let Some(cap) = capture.take() {
        cap.pause();
    }

    // Suspend is a quiesce transition, never a reopen trigger. Preserve the
    // previous route only for content-free diagnostics; Resume re-enumerates
    // and replaces it before capture can run again.
    if suspend_only {
        return WindowsLifecycleRecoveryOutcome {
            retry_count: 0,
            first_buffer_ms: None,
            fallback_reason: None,
            outcome: "suspended",
            route: last_capture_route.clone(),
            seed_samples: Vec::new(),
        };
    }

    // 2. Re-enumerate, resolve route, reopen once, then bounded retry.
    // `open_capture` already re-enumerates and applies the saved/explicit
    // vs. system-default resolution (`resolve_capture_route`), including the
    // "saved microphone unavailable" no-silent-fallback error text.
    const MAX_RETRIES: u32 = 2;
    let mut retry_count = 0u32;
    let mut last_error: Option<String> = None;
    let mut opened: Option<(WorkerCapture, Option<CaptureRouteSignature>)> = None;
    loop {
        match open_capture(saved_device) {
            Ok((mut cap, route)) => {
                cap.flush();
                if let Err(error) = cap.resume() {
                    last_error = Some(format!("capture resume: {error}"));
                } else {
                    opened = Some((cap, route));
                    break;
                }
            }
            Err(error) => last_error = Some(error),
        }
        if retry_count >= MAX_RETRIES {
            break;
        }
        retry_count += 1;
        std::thread::sleep(Duration::from_millis(150));
    }

    match opened {
        Some((mut cap, route)) => {
            let settle_ms = capture_settle_budget_ms(route.as_ref(), retry_count > 0);
            let first_buffer_started = Instant::now();
            let first = wait_for_first_capture_buffer(&mut cap, settle_ms, decision_clock);
            let first_buffer_ms = Some(
                first_buffer_started
                    .elapsed()
                    .as_millis()
                    .min(u64::MAX as u128) as u64,
            );
            let healthy =
                matches!(first, Ok(ref first) if !capture_first_buffer_requires_reopen(first));
            // Only a healthy `Data` seed is real, useful PCM; `capture_first_
            // buffer_requires_reopen` already rejects empty/non-finite data,
            // so a healthy outcome here is always `Ok(CaptureFirstBuffer::
            // Data(_))`. The caller decides whether recording is in flight
            // and only then feeds this into `buffer`/VAD bookkeeping.
            let seed_samples = if healthy {
                match &first {
                    Ok(CaptureFirstBuffer::Data(samples)) => samples.clone(),
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            };
            let outcome = if !healthy {
                "failed"
            } else if recording || wake_listening {
                "recording_resumed"
            } else {
                cap.pause();
                "idle_prepared"
            };
            *capture = Some(cap);
            *last_capture_route = route.clone();
            WindowsLifecycleRecoveryOutcome {
                retry_count,
                first_buffer_ms,
                fallback_reason: if healthy {
                    None
                } else {
                    Some(
                        first
                            .as_ref()
                            .map(capture_first_buffer_outcome)
                            .unwrap_or("capture_stream_read_failed")
                            .to_string(),
                    )
                },
                outcome,
                route,
                seed_samples,
            }
        }
        None => {
            let saved_unavailable = last_error
                .as_deref()
                .is_some_and(|error| error.contains("is unavailable"));
            let permission_blocked = last_error.as_deref().is_some_and(|error| {
                error.contains("Access is denied")
                    || error.contains("0x80070005")
                    || error.contains("-2147024891")
            });
            WindowsLifecycleRecoveryOutcome {
                retry_count,
                first_buffer_ms: None,
                fallback_reason: last_error,
                outcome: if saved_unavailable {
                    "saved_device_unavailable"
                } else if permission_blocked {
                    "permission_blocked"
                } else {
                    "failed"
                },
                route: None,
                seed_samples: Vec::new(),
            }
        }
    }
}

/// Builds the content-free `windows_capture_lifecycle` record
/// (docs/WINDOWS_IMPLEMENTATION_SET.md Wave W1 route/backend diagnostics).
/// MUST NOT include endpoint id or device name: `route.name`/`platform_id`
/// are deliberately never read here.
pub(crate) fn windows_capture_lifecycle_payload(
    generation: u64,
    cause: &'static str,
    selection: &'static str,
    route: Option<&CaptureRouteSignature>,
    recording_intent: bool,
    retry_count: u32,
    first_buffer_ms: Option<u64>,
    fallback_reason: Option<&str>,
    outcome: &'static str,
) -> serde_json::Value {
    json!({
        "event": "windows_capture_lifecycle",
        "generation": generation,
        "cause": cause,
        "requested_backend": "cpal_wasapi",
        "actual_backend": "cpal_wasapi",
        "selection": selection,
        "route_transport": route.map(|route| format!("{:?}", route.transport)),
        "route_form_factor": route.map(|route| format!("{:?}", route.form_factor)),
        "sample_rate": route.map(|route| route.native_rate),
        "channels": route.map(|route| route.channels),
        "sample_format": route.map(|route| route.sample_format.clone()),
        "recording_intent": recording_intent,
        "retry_count": retry_count,
        "first_buffer_ms": first_buffer_ms,
        "fallback_reason": fallback_reason,
        "outcome": outcome,
    })
}

/// Reuses the existing bounded, best-effort diagnostics queue
/// (`emit_command_probe` / `append_command_probe_event`) rather than adding a
/// second upload path; still gated on `diagnostics_enabled` and still
/// redacted by `rightkit_dictation_core::redact_diagnostic_event` before it ever
/// touches disk.
pub(crate) fn emit_windows_capture_lifecycle(payload: serde_json::Value) {
    emit_command_probe(payload);
}
