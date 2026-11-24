#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{
    capture_device::*, capture_recovery::*, probe_decisions::*, recording_session::*,
    streaming_commit::*, transcript_text::*, unified_probe_lane::*, wake_listener::*,
    windows_lifecycle::*, worker_api::*, worker_clock::*,
};

mod command_results;
mod recording_finish;
mod recording_pump;
mod recording_start;
mod streaming;
use streaming::StreamLane;

/// How a worker pass continues after a phase. `SkipPass` ends the current
/// pass before the result drains and audio pump, exactly like the `continue`
/// statements it replaced.
enum Flow {
    Proceed,
    SkipPass,
    Shutdown,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Decoding {
    /// The recording will be transcribed with its retired generation.
    Keep,
    /// The recording is discarded; cancel any decode already running.
    Cancel,
}

struct PendingReload {
    id: u64,
    generation: Option<u64>,
    session_id: Option<String>,
    start_fallback: bool,
    reply: Option<Sender<Result<bool, String>>>,
    reload_epoch: u64,
}

pub(super) fn reload_failure_applies(
    start_fallback: bool,
    pending_generation: Option<u64>,
    pending_session_id: Option<&str>,
    current_generation: u64,
    current_session_id: Option<&str>,
) -> bool {
    start_fallback
        && pending_generation == Some(current_generation)
        && pending_session_id
            .zip(current_session_id)
            .is_some_and(|(expected, current)| expected == current)
}

/// The single owner of dictation state. Every decision to start, stop, or
/// fail a recording is made here on the worker thread; the ASR executor and
/// capture stream only supply evidence.
pub(crate) struct Worker {
    asr_executor: AsrExecutor,
    asr_executor_failed: bool,
    focus: Arc<Mutex<FocusTracker>>,
    event_tx: Sender<WorkerEvent>,
    decision_clock: Arc<dyn worker_clock::WorkerClock>,
    cmd_rx: Receiver<WorkerCmd>,
    /// mpsc has no "peek and put back", so a command pulled via `try_recv` to
    /// gate a probe launch (see `probe_gate_clear`) is stashed here instead of
    /// dropped. `handle_next_command` checks it FIRST, so a stashed command is handled
    /// on the very next pass and no command is ever lost or reordered.
    pending_cmd_peek: Option<WorkerCmd>,
    /// Holds the Silero session; reset per recording, never rebuilt.
    speech_vad: crate::vad::SpeechVad,
    capture: CaptureDevice,
    windows_lifecycle: WindowsLifecycle,
    wake: WakeListener,
    /// Developer-mode streaming insertion for the active recording.
    stream: Option<StreamLane>,
    /// Bumped whenever a recording starts or ends. Outlives each recording so
    /// late ASR results from a retired recording are recognised as stale.
    recording_generation: u64,
    /// Bumped whenever the native KWS stream is reset; same staleness role.
    kws_stream_generation: u64,
    kws_native_origin_sample: Option<usize>,
    session: Option<RecordingSession>,
    pending_reloads: Vec<PendingReload>,
    reload_epoch: u64,
    reload_ready_generation: Option<u64>,
    cancel_epoch: Arc<std::sync::atomic::AtomicU64>,
    handled_cancel_epoch: u64,
    wait_signal: Arc<WorkerWaitSignal>,
    last_drained_signal: u64,
    /// The shortcut press that started the active recording (`StartRecording::start_attempt`).
    session_attempt: Option<u64>,
    /// When capture of the active recording began (`CaptureStart::origin`); maps an instant to a
    /// sample count on its audio.
    session_capture_origin: Option<Instant>,
    /// The active recording was sealed at the user's release (`CaptureSeal`, or the stop sealing
    /// itself): nothing more is appended to it, no more streaming partials are produced and the
    /// per-pass audio work is skipped. Cleared at every start.
    session_sealed: bool,
    /// Immutable proof kind & edge for this exact attempt/session.
    session_seal_provenance: Option<CaptureRelease>,
    /// The seal's final drain failed: the clip is NOT sealed and no cut applies. Nothing more is
    /// pumped, no further seal is attempted and the stop finalizes the buffer as it is. Cleared at
    /// every start.
    session_seal_failed: bool,
    /// Exact edge failed: successful finalization is forbidden for this session.
    session_exact_failed: bool,
}

pub(crate) fn worker_main(
    models_dir: PathBuf,
    ep: AsrEp,
    focus: Arc<Mutex<FocusTracker>>,
    cmd_rx: Receiver<WorkerCmd>,
    event_tx: Sender<WorkerEvent>,
    ready_tx: Sender<Result<(), String>>,
    decision_clock: Arc<dyn worker_clock::WorkerClock>,
    cancel_epoch: Arc<std::sync::atomic::AtomicU64>,
    wait_signal: Arc<WorkerWaitSignal>,
) {
    // The app index / shortcut refresh runs at every engine start.
    crate::app_launch::prewarm();
    let asr_executor =
        match AsrExecutor::spawn_with_wait_signal(models_dir.clone(), ep, Arc::clone(&wait_signal))
        {
            Ok(executor) => executor,
            Err(message) => {
                tracing::error!("ASR executor failed: {message}");
                let _ = ready_tx.send(Err(message.clone()));
                let _ = event_tx.send(WorkerEvent::StartFailed { message });
                return;
            }
        };
    // Warm local polish off first-dictation path after ASR is resident.
    let polish_warm = Instant::now();
    crate::text_polish::warm();
    tracing::info!(
        "worker polish warmup ok in {} ms",
        polish_warm.elapsed().as_millis()
    );
    let speech_vad = crate::vad::SpeechVad::new();

    // Signal ready as soon as the engine itself (ASR + local polish) is
    // resident — the caller of `spawn_worker` blocks on this (`ready_rx.recv()`
    // in `spawn_worker_with_clock`) and that block sits directly on the
    // launch/UI-reveal critical path (the launch splash stays up until the
    // engine reports warm). Microphone capture warmup below is a nice-to-have
    // that shaves the first-recording latency, not a precondition for the app
    // being usable — StartRecording already tolerates a cold `capture` (it
    // opens on demand), so it must not delay this signal. Mic warmup below
    // runs on this already-background worker thread AFTER the caller has been
    // unblocked.
    let _ = ready_tx.send(Ok(()));

    // Warm CoreAudio/WASAPI during engine startup, then keep the stream paused.
    // The first StartRecording reuses this resident stream, avoiding the measured
    // multi-second first-callback gap that can drop opening speech. Startup audio
    // is discarded immediately and is never treated as a recording. Runs AFTER
    // the ready signal above so it can never delay UI presentation; it still
    // completes before the command loop starts, so it still fully warms the
    // stream before the earliest possible StartRecording.
    let capture = CaptureDevice::prewarm();
    let initial_signal = wait_signal.snapshot();
    let mut worker = Worker {
        asr_executor,
        asr_executor_failed: false,
        focus,
        event_tx,
        decision_clock,
        cmd_rx,
        pending_cmd_peek: None,
        speech_vad,
        capture,
        windows_lifecycle: WindowsLifecycle::register(Arc::clone(&wait_signal)),
        wake: WakeListener::new(),
        stream: None,
        recording_generation: 0,
        kws_stream_generation: 0,
        kws_native_origin_sample: None,
        session: None,
        pending_reloads: Vec::new(),
        reload_epoch: 0,
        reload_ready_generation: None,
        cancel_epoch,
        handled_cancel_epoch: 0,
        wait_signal,
        last_drained_signal: initial_signal,
        session_attempt: None,
        session_capture_origin: None,
        session_sealed: false,
        session_seal_provenance: None,
        session_seal_failed: false,
        session_exact_failed: false,
    };
    worker.run();
}

/// Opaque cut of one capture: the point the user's release ends the clip. Today it is the release
/// instant (`Instant`), mapped to a sample count from the capture origin by `seal_session`. With
/// rightkit-audio 0.3.8 it wraps `CaptureCutToken` (minted by `CaptureCutHandle::cut_now` at the
/// release, with the handle from `begin_precise_epoch` taken before the capture resumed) and the
/// instant arithmetic in `seal_session` goes away (see HANDOFF, "When audio 0.3.8 publishes").
#[derive(Clone, Copy)]
pub(crate) struct CaptureCut {
    released_at: Instant,
}

impl CaptureCut {
    pub(crate) fn at_release(released_at: Instant) -> Self {
        Self { released_at }
    }
}

/// What the final drain of a sealed capture returns.
pub(crate) enum FinalClip {
    /// Everything captured since the last read, up to the pause (today's `pause` + `read_f32`):
    /// appended to the session buffer, which is then cut at the release by instant.
    Tail(Vec<f32>),
    /// The complete, already cut clip (`seal_precise`, audio 0.3.8): REPLACES the accumulated
    /// session buffer; no instant cut applies. Not produced before 0.3.8.
    #[allow(dead_code)]
    Complete(Vec<f32>),
}

/// Freeze and drain the capture for a seal: PAUSE the device first, then read the audio of the
/// clip. No flush before the drain. This is the one place that talks to the audio crate for a
/// seal, so its precise API (0.3.8: `seal_precise(&CaptureCutToken, timeout)`) replaces the body
/// and returns `FinalClip::Complete` without touching `seal_session`. Today it is the existing
/// `pause` + `read_f32` ordering, so the resampler's held remainder (up to ~20 ms) is not included.
/// An error is returned, never swallowed: the caller must not report the clip sealed, and nothing
/// is appended after the release.
fn drain_capture_final(cap: &mut WorkerCapture, _cut: &CaptureCut) -> Result<FinalClip, String> {
    cap.pause();
    cap.read_f32(usize::MAX).map(FinalClip::Tail)
}

impl Worker {
    fn run(&mut self) {
        loop {
            // Previous lifecycle has consumed its samples & metrics. A precise epoch cannot
            // safely resume through CPAL: retire only that stream before the next command.
            if self.session.is_none() && self.session_attempt.take().is_some() {
                self.capture.stream = None;
                self.capture.cancel_warmup();
            }
            // Acknowledge only notifications present before this complete
            // drain pass. Any later producer notification forces another pass
            // before idle parking, without an unbounded stabilization loop.
            let drained_signal = self.wait_signal.snapshot();
            // Dictation owns the model from StartRecording until its final or
            // cancel completes; queued file jobs resume only after that.
            self.asr_executor
                .set_dictation_active(self.session.is_some());
            self.service_warmup();
            if matches!(self.drain_reload_results(), Flow::SkipPass) {
                continue;
            }
            match self.handle_next_command() {
                Flow::Shutdown => break,
                Flow::SkipPass => continue,
                Flow::Proceed => {}
            }
            if matches!(self.drain_reload_results(), Flow::SkipPass) {
                continue;
            }
            self.drain_opening_results();
            self.drain_probe_results();
            self.poll_windows_lifecycle();
            self.wake
                .drain_results(&self.asr_executor, &self.capture.stream, &self.event_tx);
            self.wake
                .pump_audio(&self.asr_executor, &mut self.capture.stream, &self.event_tx);
            // A sealed recording takes no more audio and produces no more partials; it waits for
            // its stop (which finalizes the sealed buffer).
            if self.session.is_some() && !self.session_sealed && !self.session_seal_failed {
                self.pump_recording();
                self.pump_stream();
            }
            self.last_drained_signal = drained_signal;
        }
    }

    /// Seal the active recording at `cut` (the instant the user released the shortcut, or now for
    /// a stop that arrives unsealed): drain what the ring holds into the session, cut the buffer
    /// to the sample count of `cut` measured from when capture began, discard whatever the device
    /// delivers after that and stop all further audio work on this session (`session_sealed`:
    /// the pass skips `pump_recording` and `pump_stream`). The stop that follows finalizes the
    /// sealed buffer; its own tail read finds the ring empty. The device delivers a little behind
    /// wall time, so the cut never removes audio captured before the release; it removes only what
    /// arrived after it. An unknown capture origin, a cut at or before the capture began (nothing
    /// earlier exists to keep) keeps ZERO samples (the clip is empty: every sample was captured
    /// after the release, the cold seed included), a replay stream or an unknown capture origin
    /// applies no cut, and the recording is still sealed. Idempotent. Returns whether the clip is
    /// sealed: `false` when the final drain failed (the clip is then marked unsealed:
    /// `session_seal_failed`, no cut is applied, the stop finalizes the buffer as it is) or when
    /// there is no session.
    fn seal_session(&mut self, release: CaptureRelease) -> bool {
        if let CaptureRelease::Exact(edge) = release {
            if self.session_sealed {
                if self.session_seal_provenance.as_ref()
                    == Some(&CaptureRelease::Exact(edge.clone()))
                    && edge.is_ok()
                {
                    return true;
                }
                return self.reject_exact_seal("sealed capture identity or edge differs".into());
            }
            if self.session_exact_failed || self.session.is_none() {
                return false;
            }
            let sealed_edge = edge.clone();
            let clip = edge.and_then(|edge| self.capture.seal_exact(edge));
            match clip {
                Ok(clip) => {
                    self.session.as_mut().expect("session checked").buffer = clip;
                    self.session_seal_provenance = Some(CaptureRelease::Exact(sealed_edge));
                    self.session_sealed = true;
                    return true;
                }
                Err(error) => {
                    return self.reject_exact_seal(error);
                }
            }
        }
        let CaptureRelease::Legacy(cut) = release else {
            unreachable!()
        };
        if self.session_sealed {
            return matches!(
                &self.session_seal_provenance,
                Some(CaptureRelease::Legacy(_))
            );
        }
        if self.session_seal_failed {
            return false;
        }
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        let origin = self.session_capture_origin;
        if let Some(cap) = &mut self.capture.stream {
            // Freeze first: pause the device so nothing more is captured for this clip. Then drain
            // everything captured before the pause into the session before anything is discarded;
            // the cut below removes what arrived after the release. The stop that follows finds
            // the clip frozen and its tail empty (`close_capture_at_stop` discards nothing that
            // belongs here). A failed drain is never reported as sealed.
            let cut = CaptureCut::at_release(cut);
            let mut instant_cut = true;
            match drain_capture_final(cap, &cut) {
                Ok(FinalClip::Tail(chunk)) => {
                    if !chunk.is_empty() {
                        let _ =
                            session.ingest(&chunk, &mut self.speech_vad, self.decision_clock.now());
                    }
                }
                Ok(FinalClip::Complete(clip)) => {
                    // The precise seal returns the whole clip, already cut: it replaces what was
                    // accumulated, and no cut by instant follows.
                    session.buffer = clip;
                    instant_cut = false;
                }
                Err(error) => {
                    // Not sealed, no success: nothing more is appended after the release, and
                    // the stop finalizes the buffer as it is.
                    tracing::warn!(%error, "capture seal drain failed; clip left unsealed");
                    self.session_seal_failed = true;
                    return false;
                }
            }
            if let (true, false, Some(origin)) = (instant_cut, cap.is_replay(), origin) {
                // A cut at or before the capture origin maps to sample 0: nothing of this clip
                // was spoken before the release (`checked_duration_since` is `None` for an
                // earlier instant).
                let limit = cut
                    .released_at
                    .checked_duration_since(origin)
                    .map_or(0, |elapsed| {
                        (elapsed.as_secs_f64() * SAMPLE_RATE as f64) as usize
                    });
                if limit < session.buffer.len() {
                    tracing::info!(
                        kept_samples = limit,
                        dropped_samples = session.buffer.len() - limit,
                        "recording audio sealed at the shortcut release"
                    );
                    session.buffer.truncate(limit);
                }
            }
        }
        self.session_seal_provenance = Some(CaptureRelease::Legacy(cut));
        self.session_sealed = true;
        true
    }

    fn reject_exact_seal(&mut self, error: String) -> bool {
        tracing::warn!(%error, "exact capture seal rejected; recording aborted");
        self.capture.pause();
        self.session_seal_failed = true;
        self.session_exact_failed = true;
        let failed_session = self.session.as_ref().map(|session| session.id.clone());
        let _ = self.retire_session(Decoding::Cancel);
        if let Some(session_id) = failed_session {
            let _ = self.event_tx.send(WorkerEvent::CaptureFailed {
                session_id,
                message: "Capture release could not be sealed; recording discarded.".into(),
            });
        }
        false
    }

    /// Every worker pass: the shortcut-press warm-up belongs to an idle stream only, and the
    /// worker owns its expiry. A suspended microphone must stay down, so its warm-up is ended; a
    /// session or wake listening that took the stream over forgets the warm-up without touching
    /// the stream; otherwise a warm-up past its deadline pauses the stream. The idle wait wakes
    /// at the deadline (`CaptureDevice::warmup_deadline`), so this runs even when no command
    /// arrives.
    fn service_warmup(&mut self) {
        if self.windows_lifecycle.suspended {
            self.capture.abort_warmup();
        } else if self.session.is_some() || self.wake.is_active() {
            self.capture.cancel_warmup();
        }
        self.capture.expire_warmup_if_due();
    }

    fn queue_reload(
        &mut self,
        reply: Option<Sender<Result<bool, String>>>,
        start_fallback: bool,
    ) -> Result<u64, String> {
        let generation = self.session.as_ref().map(|_| self.recording_generation);
        let session_id = self.session.as_ref().map(|session| session.id.clone());
        let id = match self.asr_executor.request_reload() {
            Ok(id) => id,
            Err(error) => {
                if let Some(reply) = reply {
                    let _ = reply.send(Err(error.clone()));
                }
                return Err(error);
            }
        };
        self.pending_reloads.push(PendingReload {
            id,
            generation,
            session_id,
            start_fallback,
            reply,
            reload_epoch: self.reload_epoch,
        });
        Ok(id)
    }

    fn apply_reload_completion(
        &mut self,
        completion: ReloadCompletion,
        owner: Option<(&str, u64)>,
    ) -> Option<String> {
        if let Err(error) = &completion.result {
            tracing::error!(id = completion.id, error = %error, "worker ASR reload failed");
        }
        let pending = self
            .pending_reloads
            .iter()
            .position(|request| request.id == completion.id)
            .map(|index| self.pending_reloads.remove(index));
        let pending = pending?;
        let changed = completion.result == Ok(true);
        if changed {
            self.reload_epoch = self.reload_epoch.wrapping_add(1);
        }
        let current_owner = owner.is_some_and(|(session_id, generation)| {
            reload_failure_applies(
                true,
                pending.generation,
                pending.session_id.as_deref(),
                generation,
                Some(session_id),
            )
        });
        if current_owner && (changed || pending.reload_epoch != self.reload_epoch) {
            self.reset_kws_stream();
        }
        if let Some(reply) = pending.reply {
            let _ = reply.send(completion.result.clone());
        }
        if pending.start_fallback && completion.result.is_ok() && current_owner {
            self.reload_ready_generation = pending.generation;
        }
        if completion.result.is_err()
            && pending.start_fallback
            && owner.is_some_and(|(session_id, generation)| {
                reload_failure_applies(
                    true,
                    pending.generation,
                    pending.session_id.as_deref(),
                    generation,
                    Some(session_id),
                )
            })
        {
            return completion.result.err();
        }
        None
    }

    fn drain_reload_results(&mut self) -> Flow {
        loop {
            let completion = match self.asr_executor.try_recv_reload() {
                Ok(Some(completion)) => completion,
                Ok(None) => return Flow::Proceed,
                Err(error) => {
                    let pending = std::mem::take(&mut self.pending_reloads);
                    let owner = self
                        .session
                        .as_ref()
                        .map(|session| (session.id.clone(), self.recording_generation));
                    let mut start_failure = None;
                    for request in pending {
                        if request.start_fallback
                            && owner.as_ref().is_some_and(|(session_id, generation)| {
                                reload_failure_applies(
                                    true,
                                    request.generation,
                                    request.session_id.as_deref(),
                                    *generation,
                                    Some(session_id),
                                )
                            })
                        {
                            start_failure = Some(error.clone());
                        }
                        if let Some(reply) = request.reply {
                            let _ = reply.send(Err(error.clone()));
                        }
                    }
                    if let Some(error) = start_failure {
                        let session_id =
                            owner.map(|(session_id, _)| session_id).unwrap_or_default();
                        return self.fail_recording_start(&session_id, error);
                    }
                    return Flow::Proceed;
                }
            };
            let owner = self
                .session
                .as_ref()
                .map(|session| (session.id.clone(), self.recording_generation));
            if let Some(error) = self.apply_reload_completion(
                completion,
                owner
                    .as_ref()
                    .map(|(session_id, generation)| (session_id.as_str(), *generation)),
            ) {
                let session_id = owner.map(|(session_id, _)| session_id).unwrap_or_default();
                return self.fail_recording_start(
                    &session_id,
                    format!("ASR reload ({}): {error}", asr_reload_key()),
                );
            }
        }
    }

    fn wait_for_reload_generation(
        &mut self,
        generation: u64,
        session_id: &str,
    ) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(61);
        loop {
            let waiting = self
                .pending_reloads
                .iter()
                .any(|request| request.generation == Some(generation));
            if !waiting {
                if self.cancel_epoch.load(std::sync::atomic::Ordering::Acquire)
                    != self.handled_cancel_epoch
                {
                    return Err("recording finalization cancelled".to_string());
                }
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("ASR reload did not complete before final decode".to_string());
            }
            if self.cancel_epoch.load(std::sync::atomic::Ordering::Acquire)
                != self.handled_cancel_epoch
            {
                return Err("recording finalization cancelled".to_string());
            }
            match self.asr_executor.try_recv_reload()? {
                Some(completion) => {
                    if let Some(error) =
                        self.apply_reload_completion(completion, Some((session_id, generation)))
                    {
                        return Err(format!("ASR reload ({}): {error}", asr_reload_key()));
                    }
                }
                None => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }

    fn transcribe_final_result(
        &self,
        generation: u64,
        audio: Vec<f32>,
    ) -> Result<rightkit_dictation_inference::TranscriptionResult, String> {
        self.asr_executor.transcribe_final_result(
            generation,
            audio,
            &self.cancel_epoch,
            self.handled_cancel_epoch,
        )
    }

    fn transcribe_final_control_result(
        &self,
        generation: u64,
        audio: Vec<f32>,
        onset_sample: usize,
        intent: rightkit_text_polish::text_pipeline::ControlIntent,
    ) -> Result<FinalControlResult, String> {
        self.asr_executor.transcribe_final_control_result(
            generation,
            audio,
            onset_sample,
            intent,
            &self.cancel_epoch,
            self.handled_cancel_epoch,
        )
    }

    fn reset_kws_stream(&mut self) {
        self.kws_stream_generation = self.kws_stream_generation.wrapping_add(1);
        self.kws_native_origin_sample = None;
        self.asr_executor.reset_probe_stream();
    }

    /// Advance the recording generation and return the one being retired.
    fn retire_generation(&mut self) -> u64 {
        let retired = self.recording_generation;
        self.recording_generation = self.recording_generation.wrapping_add(1);
        retired
    }

    /// Retire the live recording: advance the generation so its late results
    /// are stale, drop queued ASR work, and for a discard also cancel
    /// in-flight decoding. Returns the session and its generation, which a
    /// finalize passes to the final decode.
    fn retire_session(&mut self, decoding: Decoding) -> Option<(RecordingSession, u64)> {
        let session = self.session.take()?;
        self.stream = None;
        let generation = self.retire_generation();
        self.asr_executor.clear_pending();
        if decoding == Decoding::Cancel {
            self.asr_executor.cancel_recording(generation);
        }
        Some((session, generation))
    }

    fn handle_next_command(&mut self) -> Flow {
        let next_cmd = match self.pending_cmd_peek.take() {
            Some(cmd) => Ok(cmd),
            // A sealed recording does no audio work, so (like idle) it parks on the command and
            // result signal instead of polling; its stop arrives as a command.
            None if (self.session.is_some()
                && !self.session_sealed
                && !self.session_seal_failed)
                || self.wake.is_active() =>
            {
                match self.cmd_rx.try_recv() {
                    Ok(command) => Ok(command),
                    Err(std::sync::mpsc::TryRecvError::Empty) => Err(RecvTimeoutError::Timeout),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        Err(RecvTimeoutError::Disconnected)
                    }
                }
            }
            None => Self::wait_for_idle_command(
                &self.cmd_rx,
                &self.wait_signal,
                self.last_drained_signal,
                self.capture.warmup_deadline(),
            ),
        };
        match next_cmd {
            Ok(WorkerCmd::StartWakeListen { reply, .. }) if self.windows_lifecycle.suspended => {
                let _ = reply.send(Err(
                    "microphone is suspended; retry after Windows resumes".to_string()
                ));
                Flow::SkipPass
            }
            Ok(WorkerCmd::StartRecording {
                session_id,
                start_attempt,
            }) if self.windows_lifecycle.suspended => {
                // The suspended microphone stays down: end a warm stream of this press too. The
                // press is over (its start failed), so a late warm-up of it is closed.
                self.capture.abort_warmup();
                self.capture.close_attempt(start_attempt);
                let _ = self.event_tx.send(WorkerEvent::CaptureFailed {
                    session_id,
                    message: "microphone is suspended; retry after Windows resumes".to_string(),
                });
                Flow::SkipPass
            }
            Ok(WorkerCmd::StartWakeListen { threshold, reply }) => {
                self.start_wake_listen(threshold, reply)
            }
            Ok(WorkerCmd::StopWakeListen { reply }) => {
                let fire_count = self.wake.end();
                self.asr_executor.stop_idle_wake();
                if self.session.is_none() {
                    self.capture.pause();
                }
                let _ = reply.send(fire_count);
                Flow::Proceed
            }
            Ok(WorkerCmd::OpenFollowUp { reply, .. }) if self.windows_lifecycle.suspended => {
                let _ = reply.send(Err(
                    "microphone is suspended; retry after Windows resumes".to_string()
                ));
                Flow::SkipPass
            }
            Ok(WorkerCmd::OpenFollowUp { window, reply }) => self.open_follow_up(window, reply),
            Ok(WorkerCmd::CloseFollowUp { reply }) => {
                let was_open = self.wake.close_follow_up();
                if was_open && self.session.is_none() && !self.wake.is_active() {
                    self.capture.pause();
                }
                let _ = reply.send(was_open);
                Flow::Proceed
            }
            Ok(WorkerCmd::ReloadModel { reply }) => {
                // Proactive switch (settings changed) — load+warm off the hotkey
                // path so the next recording is instant. The coordinator owns
                // one serialized reload; this worker keeps draining capture.
                self.asr_executor.clear_pending();
                if let Err(error) = self.queue_reload(reply, false) {
                    tracing::error!("worker ASR reload request failed: {error}");
                }
                Flow::Proceed
            }
            Ok(WorkerCmd::StartRecording {
                session_id,
                start_attempt,
            }) => self.start_recording(session_id, start_attempt),
            Ok(WorkerCmd::CaptureWarmup { attempt }) => {
                // Idle stream only: a recording, wake listening or a suspended mic already owns
                // (or forbids) delivery, and the capture device ignores a warm-up it cannot
                // adopt or whose attempt is already closed.
                if self.session.is_none()
                    && !self.wake.is_active()
                    && !self.windows_lifecycle.suspended
                {
                    self.capture.warmup(
                        attempt,
                        crate::settings::input_device(),
                        EARLY_CAPTURE_MAX,
                    );
                }
                Flow::SkipPass
            }
            Ok(WorkerCmd::CaptureRevoke { attempt }) => {
                // Explicit revoke (a cancel or stop with no session yet, a failed press, a stop or
                // cancel of the press's session): runs with or without a session, so a warm stream
                // is paused even when there is nothing to stop, and a `StartRecording` of this
                // attempt that is still behind it in the queue will not record
                // (`start_recording` checks `is_revoked`).
                self.capture.revoke_attempt(attempt);
                Flow::SkipPass
            }
            Ok(WorkerCmd::CaptureSeal {
                attempt,
                session_id,
                released_at,
                reply,
            }) => {
                // Same ordered path as the stop. The active recording of this attempt (and
                // session, when named) is sealed now; a start that has not run yet gets the
                // release held; an ended press or another session is a no-op. Never closes or
                // fences the attempt.
                let active = self.session.as_ref().map(|session| session.id.clone());
                let outcome = if active.is_some()
                    && self.session_attempt == Some(attempt)
                    && session_id
                        .as_ref()
                        .is_none_or(|id| Some(id) == active.as_ref())
                {
                    // A failed final drain is never reported as sealed.
                    if self.seal_session(released_at) {
                        "sealed"
                    } else {
                        "unsealed"
                    }
                } else if active.is_some()
                    && self.session_attempt == Some(attempt)
                    && matches!(&released_at, CaptureRelease::Exact(_))
                {
                    // Named session mismatches never receive an exact no-op acknowledgement.
                    "unsealed"
                } else {
                    match self.capture.note_release(attempt, session_id, released_at) {
                        Ok(true) => "held",
                        Ok(false) => "noop",
                        Err(error) => {
                            tracing::warn!(%error, attempt, "held capture release rejected");
                            self.capture.revoke_attempt(Some(attempt));
                            "unsealed"
                        }
                    }
                };
                let _ = reply.send(outcome);
                Flow::SkipPass
            }
            Ok(WorkerCmd::StopRecording { send_enter }) => {
                // A stop that was not preceded by a seal seals the recording itself first, at the
                // moment it is processed; the finalize below then reads only the sealed buffer.
                if self.session.is_some() && !self.session_sealed && !self.session_seal_failed {
                    self.seal_session(CaptureRelease::Legacy(Instant::now()));
                }
                self.stop_recording(send_enter)
            }
            Ok(WorkerCmd::Cancel) => {
                // Count consumed cancels, not the sender's latest epoch. A
                // Cancel queued before Start is processed must still abort a
                // later wait belonging to that earlier Start.
                self.handled_cancel_epoch = self.handled_cancel_epoch.wrapping_add(1);
                self.cancel_recording();
                Flow::Proceed
            }
            Ok(WorkerCmd::TranscribeFile { path, reply }) => self.transcribe_file(
                path,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                Arc::new(FileProgress::default()),
                false,
                reply,
            ),
            Ok(WorkerCmd::TranscribeFileJob {
                path,
                cancel,
                progress,
                verbatim,
                reply,
            }) => self.transcribe_file(path, cancel, progress, verbatim, reply),
            Ok(WorkerCmd::FailAllFileJobs { code }) => {
                self.asr_executor.fail_all_file_jobs(&code);
                Flow::Proceed
            }
            Ok(WorkerCmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                self.asr_executor.shutdown();
                if let Some(cap) = &mut self.capture.stream {
                    cap.stop();
                }
                tracing::info!("worker shutting down");
                Flow::Shutdown
            }
            Err(RecvTimeoutError::Timeout) => Flow::Proceed,
        }
    }

    /// Wait while no capture or wake work needs a cadence. Commands and
    /// asynchronous worker/lifecycle results share `wait_signal`; a signal
    /// returns control to the normal drain pass so no result lane is stranded.
    pub(crate) fn wait_for_idle_command(
        cmd_rx: &Receiver<WorkerCmd>,
        wait_signal: &WorkerWaitSignal,
        last_drained_signal: u64,
        warmup_deadline: Option<Instant>,
    ) -> Result<WorkerCmd, RecvTimeoutError> {
        match cmd_rx.try_recv() {
            Ok(command) => return Ok(command),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err(RecvTimeoutError::Disconnected)
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        let observed = wait_signal.snapshot();
        if observed != last_drained_signal {
            return Err(RecvTimeoutError::Timeout);
        }
        match cmd_rx.try_recv() {
            Ok(command) => return Ok(command),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err(RecvTimeoutError::Disconnected)
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        if wait_signal.snapshot() != observed {
            return Err(RecvTimeoutError::Timeout);
        }
        // A live warm-up bounds the park: the worker wakes at its deadline so the expiry check at
        // the top of the loop runs even when no command or notification arrives.
        match warmup_deadline {
            Some(deadline) => wait_signal.wait_for_change_until(observed, deadline),
            None => wait_signal.wait_for_change(observed),
        }
        match cmd_rx.try_recv() {
            Ok(command) => Ok(command),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(RecvTimeoutError::Disconnected),
            Err(std::sync::mpsc::TryRecvError::Empty) => Err(RecvTimeoutError::Timeout),
        }
    }

    /// Open the follow-up window: the microphone stays open and the worker polls (it does not park,
    /// see `WakeListener::is_active`) until speech starts a recording or the window ends. Refused
    /// while a recording or wake listening owns the stream.
    fn open_follow_up(&mut self, window: Duration, reply: Sender<Result<(), String>>) -> Flow {
        if self.session.is_some() || self.wake.is_active() {
            let _ = reply.send(Err(
                "cannot open a follow-up window while recording or listening".into(),
            ));
            return Flow::SkipPass;
        }
        let saved = crate::settings::input_device();
        if capture_should_reopen(
            self.capture.stream.is_none(),
            saved != self.capture.saved_device,
        ) {
            self.capture.stream = None;
            match open_capture(saved.as_deref()) {
                Ok((c, route)) => {
                    self.capture.stream = Some(c);
                    self.capture.route = route;
                    self.capture.saved_device = saved;
                }
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return Flow::SkipPass;
                }
            }
        } else if let Some(cap) = &self.capture.stream {
            if let Err(error) = cap.resume() {
                let _ = reply.send(Err(format!("capture resume: {error}")));
                return Flow::SkipPass;
            }
        }
        self.capture.cancel_warmup();
        self.wake.open_follow_up(window);
        let _ = reply.send(Ok(()));
        Flow::Proceed
    }

    fn start_wake_listen(&mut self, threshold: f32, reply: Sender<Result<(), String>>) -> Flow {
        if self.session.is_some() {
            let _ = reply.send(Err("cannot start wake listening while recording".into()));
            return Flow::SkipPass;
        }
        if self.wake.listening {
            let _ = reply.send(Ok(()));
            return Flow::SkipPass;
        }
        if let Err(error) = self.asr_executor.start_idle_wake(threshold) {
            WakeDiagnostics::startup_failure(threshold.clamp(0.01, 0.99), "wake_kws_start_error");
            let _ = reply.send(Err(error));
            return Flow::SkipPass;
        }
        let saved = crate::settings::input_device();
        if capture_should_reopen(
            self.capture.stream.is_none(),
            saved != self.capture.saved_device,
        ) {
            self.capture.stream = None;
            match open_capture(saved.as_deref()) {
                Ok((c, route)) => {
                    self.capture.stream = Some(c);
                    self.capture.route = route;
                    self.capture.saved_device = saved;
                }
                Err(error) => {
                    self.asr_executor.stop_idle_wake();
                    WakeDiagnostics::startup_failure(
                        threshold.clamp(0.01, 0.99),
                        "wake_capture_open_error",
                    );
                    let _ = reply.send(Err(error));
                    return Flow::SkipPass;
                }
            }
        } else if let Some(cap) = &self.capture.stream {
            if let Err(error) = cap.resume() {
                self.asr_executor.stop_idle_wake();
                WakeDiagnostics::startup_failure(
                    threshold.clamp(0.01, 0.99),
                    "wake_capture_resume_error",
                );
                let _ = reply.send(Err(format!("capture resume: {error}")));
                return Flow::SkipPass;
            }
        }
        self.capture.cancel_warmup();
        self.wake.begin(threshold);
        let _ = reply.send(Ok(()));
        Flow::Proceed
    }

    fn transcribe_file(
        &mut self,
        path: PathBuf,
        cancel: Arc<std::sync::atomic::AtomicBool>,
        progress: Arc<FileProgress>,
        verbatim: bool,
        reply: Sender<Result<FileTranscript, String>>,
    ) -> Flow {
        // This thread drains live mic capture and handles Stop/Cancel, so it
        // must never block on a file. Decode + queueing run on a helper
        // thread; the main ASR worker then steps the job one window at a time
        // and only while no dictation is active.
        let is_pro = crate::settings::is_pro();
        let submitter = self.asr_executor.file_submitter();
        let spawned = std::thread::Builder::new()
            .name("hr-file-transcribe".to_string())
            .spawn(move || {
                let result = (|| -> Result<FileTranscript, String> {
                    let cancelled = || cancel.load(std::sync::atomic::Ordering::Acquire);
                    if cancelled() {
                        return Err(FILE_CANCELLED.to_string());
                    }
                    // Cheap pre-decode gate: reject over-limit files before
                    // paying the full decode + RAM load.
                    if let Some(secs) = crate::file_transcribe::probe_duration_secs(&path)? {
                        rightkit_dictation_core::engine::check_duration_limit(secs, is_pro)?;
                    }
                    let audio = crate::file_transcribe::decode_to_16k_mono(&path)?;
                    // Exact backstop (probe may have returned None).
                    let secs = (audio.len() as u32) / SAMPLE_RATE;
                    rightkit_dictation_core::engine::check_duration_limit(secs, is_pro)?;
                    if cancelled() {
                        return Err(FILE_CANCELLED.to_string());
                    }
                    // Conditioning is applied once, when the job is created.
                    let conditioned = condition_file_audio(&audio);
                    drop(audio);
                    let mut transcript =
                        submitter.run(conditioned, cancel.clone(), progress, verbatim)?;
                    if !verbatim {
                        transcript.text = crate::text_polish::polish(&transcript.text);
                    }
                    Ok(transcript)
                })();
                let _ = reply.send(result);
            });
        if let Err(error) = spawned {
            // The closure (and its reply sender) is dropped; the caller sees a
            // dropped reply.
            tracing::error!("spawn file transcription thread: {error}");
        }
        Flow::Proceed
    }

    fn poll_windows_lifecycle(&mut self) {
        let recording = self.session.is_some();
        let Some(outcome) = self.windows_lifecycle.poll(
            &mut self.capture.stream,
            &mut self.capture.route,
            &mut self.capture.saved_device,
            recording,
            self.wake.listening,
            self.decision_clock.as_ref(),
        ) else {
            return;
        };
        // Recovery replaced (and paused) the stream: a warm-up from before it cannot be adopted.
        self.capture.cancel_warmup();
        // First-buffer retention (field incident class: liveness check
        // consumes real post-resume audio off the reopened stream, then
        // discards it). A recording in flight must not lose that seed the
        // same way StartRecording never loses its own first buffer. Wake-only
        // listening tracks its own pre-roll, so this is scoped to recording.
        if outcome.outcome == "recording_resumed" && !outcome.seed_samples.is_empty() {
            if let Some(session) = self.session.as_mut() {
                session.ingest(
                    &outcome.seed_samples,
                    &mut self.speech_vad,
                    self.decision_clock.now(),
                );
            }
        }
        if windows_recovery_failed(&outcome) {
            // Wake never starts a recording that was idle before suspend/an
            // endpoint event; a recording already in flight that the recovery
            // could not repair fails explicitly instead of silently going dark.
            if let Some((session, _)) = self.retire_session(Decoding::Cancel) {
                let _ = self.event_tx.send(WorkerEvent::CaptureFailed {
                    session_id: session.id,
                    message: outcome
                        .fallback_reason
                        .clone()
                        .unwrap_or_else(|| "capture recovery failed".to_string()),
                });
            }
            if self.wake.listening {
                self.wake.fail_recovery(&self.asr_executor);
            }
        }
    }
}
