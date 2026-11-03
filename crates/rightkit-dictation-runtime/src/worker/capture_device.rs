#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{capture_recovery::*, probe_decisions::*, worker_api::*, worker_clock::*};

/// The microphone stream plus the route and saved-device selection it was
/// opened for. The stream outlives recordings: startup opens it paused, and
/// recordings and wake listening resume it.
pub(crate) struct CaptureDevice {
    pub(crate) stream: Option<WorkerCapture>,
    pub(crate) route: Option<CaptureRouteSignature>,
    pub(crate) saved_device: Option<String>,
    /// Number of callback errors already emitted by worker-thread health logs.
    /// The callback only updates atomics; this watermark prevents a streaming
    /// warning from being logged once per worker pass.
    logged_async_error_count: u64,
}

/// Audio plus the decision time at which it was read, so the recording
/// observes it with the timestamp it would have had inline.
pub(crate) struct TimedAudio {
    pub(crate) samples: Vec<f32>,
    pub(crate) at: worker_clock::WorkerInstant,
}

/// What `prepare_for_recording` retained. The coordinator applies these to
/// the recording in field order.
#[derive(Default)]
pub(crate) struct CaptureStart {
    /// Delivered by the old route while the route check ran. Appended without
    /// VAD observation.
    pub(crate) route_prefix: Vec<f32>,
    /// The first healthy buffer, observed by VAD.
    pub(crate) seed: Option<TimedAudio>,
    /// Set when the start stream needed its one cold reopen; the no-speech
    /// clock restarts at this time and the session's repair budget is spent.
    pub(crate) repaired_at: Option<worker_clock::WorkerInstant>,
}

impl CaptureDevice {
    /// Startup warmup: open the selected stream paused.
    pub(crate) fn prewarm() -> Self {
        let saved_device = crate::settings::input_device();
        // Every platform keeps the stream open (paused) from startup so the first press only
        // resumes it: a cold open of the device costs 600-800 ms on Windows and loses the first
        // word. Consent is granted once during onboarding; the installer never removes the
        // package identity that holds it (hooks.nsh, #301).
        Self::prewarm_open(saved_device)
    }

    fn prewarm_open(saved_device: Option<String>) -> Self {
        let capture_prewarm_started = Instant::now();
        let (stream, route) = match open_capture_paused(saved_device.as_deref()) {
            Ok((c, route)) => {
                let elapsed_ms = capture_prewarm_started.elapsed().as_millis();
                let device = route
                    .as_ref()
                    .map(|r| r.name.as_str())
                    .unwrap_or("<default>");
                tracing::info!(
                    "capture_prewarmed source=worker_startup elapsed_ms={elapsed_ms} device={device}"
                );
                (Some(c), route)
            }
            Err(error) => {
                tracing::warn!(
                    "capture_prewarm_error source=worker_startup {error}; retrying on first recording"
                );
                (None, None)
            }
        };
        Self {
            stream,
            route,
            saved_device,
            logged_async_error_count: 0,
        }
    }

    pub(crate) fn pause(&self) {
        if let Some(cap) = &self.stream {
            cap.pause();
        }
    }

    /// Emit one worker-thread health record when the callback error counter
    /// advances after first-buffer logging. A single record may summarize
    /// several callback errors observed between worker passes.
    pub(crate) fn log_new_async_errors(&mut self) {
        let Some(cap) = self.stream.as_ref() else {
            return;
        };
        let health = cap.metrics_snapshot();
        if health.async_error_count <= self.logged_async_error_count {
            return;
        }
        self.logged_async_error_count = health.async_error_count;
        tracing::warn!(
            target: "capture_health",
            phase = "streaming",
            outcome = "async_error_observed",
            async_error_kind = health
                .last_async_error_kind
                .map(|kind| kind.as_str())
                .unwrap_or("none"),
            terminal_error_kind = health
                .terminal_error_kind
                .map(|kind| kind.as_str())
                .unwrap_or("none"),
            async_errors = health.async_error_count,
            "capture health transition"
        );
    }

    /// Open, flush, and resume the stream for a new recording, reopen it once
    /// if the OS route changed or the first buffer shows a dead stream, and
    /// return every sample read along the way. `Err` means the recording
    /// cannot start; the stream may be left open and the caller pauses it.
    ///
    /// Order matters for the first syllable: flush stale samples while
    /// paused, resume before the slow route inspection so audio accumulates,
    /// and keep audio consumed by route and liveness checks.
    pub(crate) fn prepare_for_recording(
        &mut self,
        saved: Option<String>,
        session_id: &str,
        start_received_at: Instant,
        decision_clock: &dyn WorkerClock,
    ) -> Result<CaptureStart, String> {
        let mut start = CaptureStart::default();
        // Privacy boundary: startup may leave selected stream open but
        // paused; StartRecording or explicit wake-listen resumes delivery.
        // Reuse that stream when possible; cold-open paused so shared
        // flush+resume path begins playback once after setup.
        if capture_should_reopen(self.stream.is_none(), saved != self.saved_device) {
            self.stream = None;
            let (c, route) = open_capture_paused(saved.as_deref())?;
            self.stream = Some(c);
            self.route = route;
            self.saved_device = saved.clone();
        }
        // Clear stale samples while the stream is paused. Flushing after
        // resume raced the first fresh callback and could discard a few
        // milliseconds from an immediate leading phoneme.
        if let Some(cap) = &mut self.stream {
            let discarded = cap.flush();
            if discarded > 0 {
                tracing::info!("flushed {discarded} stale input samples before record");
            }
        }
        if let Some(cap) = &self.stream {
            if let Err(error) = cap.resume() {
                // A failed start must not pin subsequent attempts to this
                // stream; reopen against current permissions/route next time.
                self.stream = None;
                self.route = None;
                self.logged_async_error_count = 0;
                return Err(capture_open_error(format!("resume: {error}")));
            }
        }
        trace_recording_start(
            session_id,
            "capture_resumed",
            start_received_at.elapsed().as_millis() as u64,
        );

        // Inspect the OS capture route only AFTER capture resumes. Device
        // enumeration can take hundreds of milliseconds, but audio
        // now accumulates in the ring during that work instead of being lost.
        // A real device/rate/channel/format change gets one cold reopen.
        // This covers CoreAudio call-route changes and the equivalent
        // WASAPI default/Bluetooth hands-free endpoint transition.
        match current_capture_route(saved.as_deref()) {
            Ok(current_route) if capture_route_changed(&self.route, &current_route) => {
                // Preserve any prefix the old route delivered while the
                // signature check ran. Shared-mode capture often contains
                // the user's first word even when another app has just
                // changed the device configuration.
                if let Some(cap) = &mut self.stream {
                    if let Ok(prefix) = cap.read_f32(usize::MAX) {
                        start.route_prefix = prefix;
                    }
                    cap.pause();
                }
                self.stream = None;
                let (mut c, route) = open_capture(saved.as_deref())?;
                c.flush();
                c.resume()
                    .map_err(|e| format!("capture route resume: {e}"))?;
                self.stream = Some(c);
                self.route = route;
                self.logged_async_error_count = 0;
                trace_recording_start(
                    session_id,
                    "capture_route_reopened",
                    start_received_at.elapsed().as_millis() as u64,
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("{e}; reusing the active capture route"),
        }

        // Wait for first frames without clipping the leading word.
        // Bluetooth gets a longer bounded settle because hands-free
        // routes can publish startup zeros before real PCM.
        let Some(cap) = &mut self.stream else {
            return Err("capture stream is not live".to_string());
        };
        let settle_ms = capture_settle_budget_ms(self.route.as_ref(), false);
        let (capture_needs_reopen, capture_health_outcome) =
            match wait_for_first_capture_buffer(cap, settle_ms, decision_clock) {
                Ok(first) => {
                    let needs_reopen =
                        !cap.is_replay() && capture_first_buffer_requires_reopen(&first);
                    let first_outcome = capture_first_buffer_outcome(&first);
                    let outcome = match first {
                        CaptureFirstBuffer::Data(seed) => {
                            if !needs_reopen {
                                start.seed = Some(TimedAudio {
                                    samples: seed,
                                    at: decision_clock.now(),
                                });
                            }
                            first_outcome
                        }
                        CaptureFirstBuffer::NoCallbacks => "capture_stream_no_callbacks",
                        CaptureFirstBuffer::NoSamples => "capture_stream_no_samples",
                        CaptureFirstBuffer::StreamError(kind) => kind.as_str(),
                    };
                    (needs_reopen, outcome)
                }
                Err(error) => {
                    tracing::error!(%error, "capture first-buffer read failed");
                    (true, "capture_stream_read_failed")
                }
            };
        self.logged_async_error_count =
            log_first_buffer_health(cap, self.route.as_ref(), capture_health_outcome, 0);
        trace_recording_start(
            session_id,
            "seed_captured",
            start_received_at.elapsed().as_millis() as u64,
        );
        // A resumed warm stream with no callbacks/samples, non-finite
        // readiness PCM, or terminal stream error gets exactly one cold
        // reopen. Digital-zero PCM remains valid quiet/noise-gated input.
        // Repair does not delay recording-state activation or pill rendering.
        if capture_needs_reopen {
            trace_recording_start(
                session_id,
                capture_health_outcome,
                start_received_at.elapsed().as_millis() as u64,
            );
            let live = self.reopen_dead_start_stream(
                saved.as_deref(),
                session_id,
                start_received_at,
                decision_clock,
                &mut start,
            );
            // The dead stream burned wall clock before the user could be
            // heard. Restart the no-speech clock so the backstop measures
            // the user's silence, not the stream's.
            start.repaired_at = Some(decision_clock.now());
            if !live {
                return Err("capture stream is not live".to_string());
            }
        }
        if self.stream.is_none() {
            return Err("capture stream is not live".to_string());
        }
        Ok(start)
    }

    /// One cold reopen of a start stream that delivered no usable audio.
    /// Stores a healthy reopened seed in `start.seed` and returns liveness.
    fn reopen_dead_start_stream(
        &mut self,
        saved: Option<&str>,
        session_id: &str,
        start_received_at: Instant,
        decision_clock: &dyn WorkerClock,
        start: &mut CaptureStart,
    ) -> bool {
        if let Some(cap) = &mut self.stream {
            cap.pause();
        }
        self.stream = None;
        let (mut reopened, route) = match open_capture(saved) {
            Ok(opened) => opened,
            Err(e) => {
                tracing::error!("capture liveness reopen failed: {e}");
                trace_recording_start(
                    session_id,
                    "capture_stream_reopen_failed",
                    start_received_at.elapsed().as_millis() as u64,
                );
                return false;
            }
        };
        reopened.flush();
        if let Err(e) = reopened.resume() {
            tracing::error!("capture liveness reopen resume failed: {e}");
            trace_recording_start(
                session_id,
                "capture_stream_reopen_failed",
                start_received_at.elapsed().as_millis() as u64,
            );
            return false;
        }
        self.route = route;
        self.logged_async_error_count = 0;
        let settle_ms = capture_settle_budget_ms(self.route.as_ref(), true);
        let reopened_first =
            wait_for_first_capture_buffer(&mut reopened, settle_ms, decision_clock);
        let reopened_outcome = reopened_first
            .as_ref()
            .map(capture_first_buffer_outcome)
            .unwrap_or("capture_stream_read_failed");
        let still_dead = match reopened_first {
            Ok(first) => {
                let unhealthy = capture_first_buffer_requires_reopen(&first);
                if let CaptureFirstBuffer::Data(seed) = first {
                    if !unhealthy {
                        start.seed = Some(TimedAudio {
                            samples: seed,
                            at: decision_clock.now(),
                        });
                    }
                }
                unhealthy
            }
            Err(error) => {
                tracing::error!(%error, "capture first-buffer read failed after reopen");
                true
            }
        };
        self.logged_async_error_count = log_first_buffer_health(
            &reopened,
            self.route.as_ref(),
            if still_dead {
                reopened_outcome
            } else {
                "recovered"
            },
            1,
        );
        self.stream = Some(reopened);
        trace_recording_start(
            session_id,
            if still_dead {
                "capture_stream_flatline_after_reopen"
            } else {
                "capture_stream_recovered_after_reopen"
            },
            start_received_at.elapsed().as_millis() as u64,
        );
        !still_dead
    }
}

fn log_first_buffer_health(
    cap: &WorkerCapture,
    route: Option<&CaptureRouteSignature>,
    outcome: &str,
    repair_attempt: u8,
) -> u64 {
    let health = cap.metrics_snapshot();
    // The cpal error callback is deliberately allocation-free and cannot log.
    // Read its bounded counter/kind snapshot here, once per liveness transition,
    // so a benign startup warning is distinguishable from a terminal stream
    // failure in installed logs.
    let async_error_kind = health
        .last_async_error_kind
        .map(|kind| kind.as_str())
        .unwrap_or("none");
    let terminal_error_kind = health
        .terminal_error_kind
        .map(|kind| kind.as_str())
        .unwrap_or("none");
    tracing::info!(
        target: "capture_health",
        phase = "first_buffer",
        outcome,
        repair_attempt,
        async_error_kind,
        terminal_error_kind,
        callbacks = health.callback_invocations,
        input_samples = health.input_samples_received,
        first_callback_ms = health.first_callback_latency_us.map(|us| us / 1000),
        async_errors = health.async_error_count,
        input_rate = cap.input_rate(),
        channels = cap.input_channels(),
        sample_format = cap.input_sample_format(),
        transport = ?route.map(|route| route.transport),
        form_factor = ?route.map(|route| route.form_factor),
        "capture health transition"
    );
    health.async_error_count
}
