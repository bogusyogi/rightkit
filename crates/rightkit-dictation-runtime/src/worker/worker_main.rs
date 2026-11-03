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
    };
    worker.run();
}

impl Worker {
    fn run(&mut self) {
        loop {
            // Acknowledge only notifications present before this complete
            // drain pass. Any later producer notification forces another pass
            // before idle parking, without an unbounded stabilization loop.
            let drained_signal = self.wait_signal.snapshot();
            // Dictation owns the model from StartRecording until its final or
            // cancel completes; queued file jobs resume only after that.
            self.asr_executor
                .set_dictation_active(self.session.is_some());
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
                .pump_audio(&self.asr_executor, &mut self.capture.stream);
            if self.session.is_some() {
                self.pump_recording();
                self.pump_stream();
            }
            self.last_drained_signal = drained_signal;
        }
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
            None if self.session.is_some() || self.wake.is_active() => {
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
            ),
        };
        match next_cmd {
            Ok(WorkerCmd::StartWakeListen { reply, .. }) if self.windows_lifecycle.suspended => {
                let _ = reply.send(Err(
                    "microphone is suspended; retry after Windows resumes".to_string()
                ));
                Flow::SkipPass
            }
            Ok(WorkerCmd::StartRecording { session_id }) if self.windows_lifecycle.suspended => {
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
                    if let Some(cap) = &self.capture.stream {
                        cap.pause();
                    }
                }
                let _ = reply.send(fire_count);
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
            Ok(WorkerCmd::StartRecording { session_id }) => self.start_recording(session_id),
            Ok(WorkerCmd::StopRecording { send_enter }) => self.stop_recording(send_enter),
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
        wait_signal.wait_for_change(observed);
        match cmd_rx.try_recv() {
            Ok(command) => Ok(command),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(RecvTimeoutError::Disconnected),
            Err(std::sync::mpsc::TryRecvError::Empty) => Err(RecvTimeoutError::Timeout),
        }
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
