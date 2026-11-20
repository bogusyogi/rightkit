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
    /// The live shortcut-press warm-up, if any. The ring has held every sample since the stream
    /// resumed; the start of the same attempt adopts them instead of flushing.
    warmup: Option<Warmup>,
    /// Highest start attempt already started, stopped, cancelled, revoked, expired or superseded.
    /// A warm-up for an attempt at or below it is ignored, so a late or reordered warm-up can
    /// never resume the microphone for a press that is already over.
    attempt_floor: u64,
    /// Highest start attempt ended by anything EXCEPT warm-up expiry: started, stopped, cancelled,
    /// revoked, aborted or superseded. `note_release` checks this, not `attempt_floor`: a warm-up
    /// that merely expired leaves its press free to cold start, so its release must still be held
    /// for that start (a start that arrives after the expiry applies it: capture then began after
    /// the release, so the cut keeps zero samples).
    closed_floor: u64,
    /// Start attempts the shell explicitly revoked (a cancel or stop with no session yet, or a
    /// press that failed). A `StartRecording` for one of them must not record: the shell already
    /// walked away from that press. This is separate from `attempt_floor`, which expiry also
    /// raises: a warm-up that merely expired leaves its press free to cold start. Bounded to the
    /// newest `REVOKED_ATTEMPTS_KEPT` ids; attempt ids only grow, so an old id is never reused.
    revoked: std::collections::VecDeque<u64>,
    /// Releases (`CaptureSeal`) that reached the worker before the `StartRecording` of their
    /// attempt ran: `(attempt, instant the release reached the engine)`. The start takes its own
    /// (`take_release`); an entry for an attempt that is closed is dropped on sight. Bounded to
    /// `RELEASES_KEPT`; attempt ids only grow, so an old entry can never match a later press.
    releases: std::collections::VecDeque<(u64, Option<String>, CaptureRelease)>,
    precise: Option<Result<rightkit_audio::capture::CaptureCutHandle, String>>,
}

/// How many explicitly revoked attempt ids the worker remembers.
const REVOKED_ATTEMPTS_KEPT: usize = 16;

/// How many not-yet-applied releases the worker remembers.
const RELEASES_KEPT: usize = 4;

/// One live warm-up. The worker loop owns the expiry: `deadline` is the instant the stream
/// actually resumed plus the bound, and every worker pass checks it.
struct Warmup {
    attempt: u64,
    deadline: Instant,
    /// When the stream actually resumed: the instant the first sample of the warm-up can have been
    /// captured. A start that adopts the warm-up uses it as the origin of the recording's audio
    /// timeline (`CaptureStart::origin`).
    resumed_at: Instant,
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
    /// When capture of this recording began: the adopted warm-up's resume instant, or the instant
    /// this start resumed the stream. A release instant maps to a sample count on the recording's
    /// audio through it (`Worker::seal_session`). `None` when it is unknown (a failed resume path
    /// never returns here), in which case no release cut applies.
    pub(crate) origin: Option<Instant>,
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
            warmup: None,
            attempt_floor: 0,
            closed_floor: 0,
            revoked: std::collections::VecDeque::new(),
            releases: std::collections::VecDeque::new(),
            precise: None,
        }
    }

    /// End attempt `attempt` for every purpose (warm-up fence and held release alike). Expiry is
    /// the one ending that raises only `attempt_floor`.
    fn close(&mut self, attempt: u64) {
        self.attempt_floor = self.attempt_floor.max(attempt);
        self.closed_floor = self.closed_floor.max(attempt);
    }

    pub(crate) fn pause(&mut self) {
        // Every pause ends a warm-up: nothing may adopt audio from a stream that stopped.
        self.warmup = None;
        if let Some(cap) = &self.stream {
            cap.pause();
        }
    }

    /// Shortcut pressed (start attempt `attempt`): flush stale samples while paused, then resume
    /// the stream so audio accumulates while the shell does its start work. The deadline is armed
    /// here, at the moment the stream actually resumed. Cheap only: a missing stream, a changed
    /// saved device or a failed resume leaves everything to `prepare_for_recording`, which opens
    /// or repairs as before; a failed resume also pauses again so no half-resumed stream is left
    /// running. An attempt at or below the floor is ignored, as is a repeat or older attempt
    /// while a newer warm-up is live; a newer attempt supersedes the live one.
    pub(crate) fn warmup(&mut self, attempt: u64, saved: Option<String>, max: Duration) {
        if attempt <= self.attempt_floor {
            tracing::info!(attempt, "capture_warmup_ignored_closed_attempt");
            return;
        }
        if let Some(live) = &self.warmup {
            if live.attempt >= attempt {
                return;
            }
            self.close(live.attempt);
            self.pause();
        }
        if capture_should_reopen(self.stream.is_none(), saved != self.saved_device) {
            return;
        }
        let Some(cap) = &mut self.stream else {
            return;
        };
        let discarded = cap.flush();
        if discarded > 0 {
            tracing::info!("flushed {discarded} stale input samples before early capture");
        }
        self.precise = Some(cap.begin_precise_epoch());
        match cap.resume() {
            Ok(()) => {
                let resumed_at = Instant::now();
                self.warmup = Some(Warmup {
                    attempt,
                    deadline: resumed_at + max,
                    resumed_at,
                });
                tracing::info!(attempt, "capture_warmup_resumed");
            }
            Err(error) => {
                cap.pause();
                tracing::warn!("capture_warmup_resume_error {error}; start will resume capture");
            }
        }
    }

    /// Forget a live warm-up without touching the stream (something else took it over).
    pub(crate) fn cancel_warmup(&mut self) {
        self.warmup = None;
    }

    /// When the worker's idle wait must wake to expire the live warm-up.
    pub(crate) fn warmup_deadline(&self) -> Option<Instant> {
        self.warmup.as_ref().map(|live| live.deadline)
    }

    /// Worker-loop expiry check, run on every pass: once the deadline has elapsed the stream is
    /// paused and the attempt closed, whether or not any command arrived. No separate timer
    /// exists, so nothing can fail to arm it.
    pub(crate) fn expire_warmup_if_due(&mut self) {
        let Some(live) = &self.warmup else {
            return;
        };
        if Instant::now() < live.deadline {
            return;
        }
        let attempt = live.attempt;
        self.attempt_floor = self.attempt_floor.max(attempt);
        self.pause();
        tracing::info!(attempt, "capture_warmup_expired");
    }

    /// End a live warm-up now and close its attempt (the microphone must stay down).
    pub(crate) fn abort_warmup(&mut self) {
        let Some(attempt) = self.warmup.as_ref().map(|live| live.attempt) else {
            return;
        };
        self.close(attempt);
        self.pause();
        tracing::info!(attempt, "capture_warmup_aborted");
    }

    /// Close start attempt `attempt` because its `StartRecording` was consumed some other way (wake
    /// handoff, suspended microphone): a late warm-up of it can no longer resume anything. It does
    /// not fence a start, and it leaves a live warm-up alone.
    pub(crate) fn close_attempt(&mut self, attempt: Option<u64>) {
        if let Some(attempt) = attempt {
            self.close(attempt);
        }
    }

    /// Explicit revoke of start attempt `attempt` (a cancel or stop with no session yet, a press
    /// that failed, a rejected start, a stop or cancel frame of its session): close the attempt,
    /// remember that it was revoked so a `StartRecording` for it does not record, and pause a warm
    /// stream of exactly this attempt. Another press's warm-up (older or newer) is left alone, so
    /// one press can never retire another's audio. `None` (a frame from a shell without early
    /// capture) changes nothing.
    pub(crate) fn revoke_attempt(&mut self, attempt: Option<u64>) {
        let Some(attempt) = attempt else {
            return;
        };
        self.close(attempt);
        if !self.revoked.contains(&attempt) {
            if self.revoked.len() >= REVOKED_ATTEMPTS_KEPT {
                self.revoked.pop_front();
            }
            self.revoked.push_back(attempt);
        }
        if self
            .warmup
            .as_ref()
            .is_some_and(|live| live.attempt == attempt)
        {
            self.pause();
            tracing::info!(attempt, "capture_warmup_revoked");
        }
    }

    /// Whether the shell explicitly revoked `attempt` (not merely let its warm-up expire). A start
    /// for such an attempt must not record.
    pub(crate) fn is_revoked(&self, attempt: Option<u64>) -> bool {
        attempt.is_some_and(|attempt| self.revoked.contains(&attempt))
    }

    /// `StartRecording` for `attempt`: take the warm-up and report whether this start adopts it.
    /// Only the warm-up of the same attempt, still inside its bound and on a live stream, is
    /// adopted. Any other live warm-up is ended so the start's own flush-then-resume runs on a
    /// paused stream. Either way the attempt is closed.
    fn claim_warmup(&mut self, attempt: Option<u64>) -> Option<Instant> {
        let live = self.warmup.take();
        if let Some(attempt) = attempt {
            self.close(attempt);
        }
        let live = live?;
        let adopt = attempt == Some(live.attempt)
            && Instant::now() < live.deadline
            && self.stream.is_some();
        if !adopt {
            self.close(live.attempt);
            if let Some(cap) = &self.stream {
                cap.pause();
            }
            return None;
        }
        Some(live.resumed_at)
    }

    /// The user released the shortcut of `attempt` at `released_at`, before that attempt's
    /// `StartRecording` ran: hold the instant for the start (`take_release`). A closed attempt
    /// (started, stopped, cancelled, revoked, superseded) has nothing to hold it for and is ignored;
    /// an attempt whose warm-up merely EXPIRED is still open (its start cold starts), so its
    /// release is held, and a held release survives the expiry.
    pub(crate) fn note_release(
        &mut self,
        attempt: u64,
        session_id: Option<String>,
        released_at: CaptureRelease,
    ) -> Result<bool, String> {
        if attempt <= self.closed_floor {
            return Ok(false);
        }
        if let Some((_, bound, held)) = self.releases.iter_mut().find(|(id, _, _)| *id == attempt) {
            if bound
                .as_ref()
                .zip(session_id.as_ref())
                .is_some_and(|(old, new)| old != new)
            {
                return Err("held capture session differs".into());
            }
            if matches!(&*held, CaptureRelease::Exact(_))
                && matches!(&released_at, CaptureRelease::Exact(_))
                && *held != released_at
            {
                return Err("held exact capture edge differs".into());
            }
            // A legitimately unknown session binds once. An omitted retry never clears it.
            if bound.is_none() {
                *bound = session_id;
            }
            // A later legacy frame cannot replace an exact edge.
            if matches!(&*held, CaptureRelease::Legacy(_))
                && matches!(&released_at, CaptureRelease::Exact(_))
            {
                *held = released_at;
            }
            return Ok(true);
        }
        if self.releases.len() >= RELEASES_KEPT {
            self.releases.pop_front();
        }
        self.releases.push_back((attempt, session_id, released_at));
        Ok(true)
    }

    /// Consume only the release for this attempt & its named session. Unknown session binds here.
    pub(crate) fn take_release(
        &mut self,
        attempt: Option<u64>,
        session_id: &str,
    ) -> Result<Option<CaptureRelease>, String> {
        let Some(attempt) = attempt else {
            return Ok(None);
        };
        let found = self
            .releases
            .iter()
            .find(|(held, _, _)| *held == attempt)
            .map(|(_, bound, release)| {
                if bound.as_deref().is_some_and(|bound| bound != session_id) {
                    Err("held capture session does not match start".into())
                } else {
                    Ok(release.clone())
                }
            })
            .transpose();
        self.releases.retain(|(held, _, _)| *held > attempt);
        found
    }

    pub(crate) fn seal_exact(
        &mut self,
        edge: rightkit_audio::capture::CaptureEdge,
    ) -> Result<Vec<f32>, String> {
        let handle = self
            .precise
            .as_ref()
            .ok_or("precise capture epoch unavailable")?
            .as_ref()
            .map_err(Clone::clone)?;
        let token = handle.cut_at(edge).map_err(|error| error.to_string())?;
        self.stream
            .as_mut()
            .ok_or("capture stream unavailable")?
            .seal_precise(&token)
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
        start_attempt: Option<u64>,
    ) -> Result<CaptureStart, String> {
        let mut start = CaptureStart::default();
        // The warm-up of this start's own press, still inside its bound: the stream has been
        // running since then and the ring holds the audio the user has spoken meanwhile.
        let warmed_origin = self.claim_warmup(start_attempt);
        let warmed = warmed_origin.is_some();
        // Privacy boundary: startup may leave selected stream open but
        // paused; StartRecording or explicit wake-listen resumes delivery.
        // Reuse that stream when possible; cold-open paused so shared
        // flush+resume path begins playback once after setup.
        let reopened = capture_should_reopen(self.stream.is_none(), saved != self.saved_device);
        if reopened {
            self.stream = None;
            let (c, route) = open_capture_paused(saved.as_deref())?;
            self.stream = Some(c);
            self.route = route;
            self.saved_device = saved.clone();
        }
        // The warm-up's samples are the start of this recording unless the stream was replaced.
        let adopt_warmup = warmed && !reopened && self.stream.is_some();
        // Clear stale samples while the stream is paused. Flushing after
        // resume raced the first fresh callback and could discard a few
        // milliseconds from an immediate leading phoneme. An adopted warm-up
        // already flushed at the press and keeps everything read since.
        if !adopt_warmup {
            if let Some(cap) = &mut self.stream {
                let discarded = cap.flush();
                if discarded > 0 {
                    tracing::info!("flushed {discarded} stale input samples before record");
                }
            }
        }
        if adopt_warmup {
            // The adopted audio began when the press resumed the stream.
            start.origin = warmed_origin;
            trace_recording_start(
                session_id,
                "capture_warmup_adopted",
                start_received_at.elapsed().as_millis() as u64,
            );
        }
        if let Some(cap) = self.stream.as_mut().filter(|_| !adopt_warmup) {
            self.precise = start_attempt.map(|_| cap.begin_precise_epoch());
            if let Err(error) = cap.resume() {
                // A failed start must not pin subsequent attempts to this
                // stream; reopen against current permissions/route next time.
                self.stream = None;
                self.route = None;
                self.logged_async_error_count = 0;
                return Err(capture_open_error(format!("resume: {error}")));
            }
            // This start's own resume: the audio timeline begins here.
            start.origin = Some(Instant::now());
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
                // One precise epoch cannot certify audio spanning different streams.
                self.precise = Some(Err("capture route changed during precise epoch".into()));
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
        self.precise = Some(Err("capture recovered during precise epoch".into()));
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
