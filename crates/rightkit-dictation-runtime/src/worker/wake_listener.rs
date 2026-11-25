#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{follow_up::*, unified_probe_lane::*, worker_api::*};

const IDLE_AUDIO_WINDOW_SAMPLES: usize = rightkit_dictation_inference::IDLE_AUDIO_WINDOW_SAMPLES;

/// Idle wake listening and the wake-to-recording handoff. `listening` and
/// `start_pending` are never true while a recording is active.
pub(crate) struct WakeListener {
    pub(crate) listening: bool,
    /// Wake fired; audio keeps accumulating in `handoff_audio` until the
    /// runtime sends StartRecording.
    pub(crate) start_pending: bool,
    threshold: f32,
    fire_count: u64,
    diagnostics: Option<WakeDiagnostics>,
    idle_sample_clock: usize,
    idle_audio_window: std::collections::VecDeque<f32>,
    idle_audio_origin: usize,
    handoff_audio: Vec<f32>,
    /// An open session (see `FollowUpWatch`). Never set while `listening`, `start_pending` or a
    /// recording is active.
    follow_up: Option<FollowUpWatch>,
}

impl WakeListener {
    pub(crate) fn new() -> Self {
        Self {
            listening: false,
            start_pending: false,
            threshold: 0.25,
            fire_count: 0,
            diagnostics: None,
            idle_sample_clock: 0,
            idle_audio_window: std::collections::VecDeque::with_capacity(IDLE_AUDIO_WINDOW_SAMPLES),
            idle_audio_origin: 0,
            handoff_audio: Vec::with_capacity(SAMPLE_RATE as usize * 2),
            follow_up: None,
        }
    }

    /// The worker must poll audio, not park, while this is true: wake listening, a wake that is
    /// waiting for its recording, or an open follow-up window.
    pub(crate) fn is_active(&self) -> bool {
        self.listening || self.start_pending || self.follow_up.is_some()
    }

    /// Open the follow-up window for `window` from now. The caller has resumed capture.
    pub(crate) fn open_follow_up(&mut self, window: Duration) {
        self.handoff_audio.clear();
        self.follow_up = Some(FollowUpWatch::open(Instant::now(), window));
    }

    /// Close the window; true when one was open. The caller pauses capture if no recording owns it.
    pub(crate) fn close_follow_up(&mut self) -> bool {
        self.follow_up.take().is_some()
    }

    /// Record a successful StartWakeListen once KWS and capture are running.
    pub(crate) fn begin(&mut self, threshold: f32) {
        self.idle_sample_clock = 0;
        self.idle_audio_window.clear();
        self.idle_audio_origin = 0;
        self.handoff_audio.clear();
        self.start_pending = false;
        self.threshold = threshold.clamp(0.01, 0.99);
        self.listening = true;
        self.diagnostics = Some(WakeDiagnostics::started(self.threshold));
    }

    /// StopWakeListen: returns the fire count for the reply. The caller stops
    /// idle KWS and pauses capture.
    pub(crate) fn end(&mut self) -> u64 {
        if let Some(diagnostics) = self.diagnostics.as_mut() {
            diagnostics.stopped(self.fire_count);
        }
        self.diagnostics = None;
        self.listening = false;
        self.start_pending = false;
        self.follow_up = None;
        self.idle_audio_window.clear();
        self.handoff_audio.clear();
        self.fire_count
    }

    /// Audio captured since the wake word, for StartRecording to seed from.
    pub(crate) fn take_handoff(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.handoff_audio)
    }

    /// Windows lifecycle recovery could not restore a live stream.
    pub(crate) fn fail_recovery(&mut self, asr_executor: &AsrExecutor) {
        self.listening = false;
        self.start_pending = false;
        asr_executor.stop_idle_wake();
        if let Some(diagnostics) = self.diagnostics.as_mut() {
            diagnostics.failed("windows_lifecycle_recovery_failed");
            diagnostics.stopped(self.fire_count);
        }
        self.diagnostics = None;
    }

    pub(crate) fn drain_results(
        &mut self,
        asr_executor: &AsrExecutor,
        capture: &Option<WorkerCapture>,
        event_tx: &Sender<WorkerEvent>,
    ) {
        loop {
            match asr_executor.try_recv_idle_wake() {
                Ok(Some(result)) => match result.hit {
                    Ok(Some((_onset, end_sample))) if self.listening => {
                        self.listening = false;
                        self.start_pending = true;
                        self.fire_count = self.fire_count.saturating_add(1);
                        asr_executor.stop_idle_wake();
                        let handoff_start =
                            rightkit_dictation_inference::wake_handoff_start(end_sample);
                        self.handoff_audio = rightkit_dictation_inference::wake_handoff_audio(
                            &self.idle_audio_window,
                            self.idle_audio_origin,
                            handoff_start,
                        );
                        let ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs_f64();
                        tracing::info!(
                            total_samples = result.total_samples,
                            decode_ms = result.decode_ms,
                            fire_count = self.fire_count,
                            "idle wake fired"
                        );
                        if let Some(diagnostics) = self.diagnostics.as_mut() {
                            diagnostics.observe_decode(result.decode_ms);
                            diagnostics.fired(self.fire_count);
                            diagnostics.stopped(self.fire_count);
                        }
                        self.diagnostics = None;
                        let _ = event_tx.send(WorkerEvent::WakeFired {
                            ts,
                            fire_count: self.fire_count,
                            threshold: self.threshold,
                            follow_up: false,
                        });
                    }
                    Ok(_) => {
                        if let Some(diagnostics) = self.diagnostics.as_mut() {
                            diagnostics.observe_decode(result.decode_ms);
                        }
                    }
                    Err(error) if self.listening => {
                        tracing::error!(error, "idle wake decode failed");
                        if let Some(diagnostics) = self.diagnostics.as_mut() {
                            diagnostics.failed("kws_decode_error");
                            diagnostics.stopped(self.fire_count);
                        }
                        self.diagnostics = None;
                        self.listening = false;
                        asr_executor.stop_idle_wake();
                        if let Some(cap) = capture {
                            cap.pause();
                        }
                    }
                    Err(_) => {}
                },
                Ok(None) => break,
                Err(error) => {
                    tracing::error!(error, "idle wake result lane failed");
                    if let Some(diagnostics) = self.diagnostics.as_mut() {
                        diagnostics.failed("kws_lane_error");
                        diagnostics.stopped(self.fire_count);
                    }
                    self.diagnostics = None;
                    self.listening = false;
                    break;
                }
            }
        }
    }

    /// One chunk inside an open follow-up window: speech starts a recording seeded with the audio
    /// heard so far (the same hand-off a wake makes, reported as a wake whose `follow_up` is set),
    /// and an expired window closes and pauses capture.
    fn pump_follow_up(
        &mut self,
        chunk: &[f32],
        capture: &WorkerCapture,
        event_tx: &Sender<WorkerEvent>,
    ) {
        let Some(watch) = self.follow_up.as_mut() else {
            return;
        };
        match watch.ingest(Instant::now(), chunk) {
            FollowUpStep::Waiting => {}
            FollowUpStep::Speech(audio) => {
                self.follow_up = None;
                self.start_pending = true;
                self.fire_count = self.fire_count.saturating_add(1);
                self.handoff_audio = audio;
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                tracing::info!(fire_count = self.fire_count, "follow-up speech began");
                let _ = event_tx.send(WorkerEvent::WakeFired {
                    ts,
                    fire_count: self.fire_count,
                    threshold: self.threshold,
                    follow_up: true,
                });
            }
            FollowUpStep::Expired => {
                self.follow_up = None;
                capture.pause();
            }
        }
    }

    /// Feed idle KWS while listening, or buffer handoff audio while the
    /// runtime has not yet sent StartRecording.
    pub(crate) fn pump_audio(
        &mut self,
        asr_executor: &AsrExecutor,
        capture: &mut Option<WorkerCapture>,
        event_tx: &Sender<WorkerEvent>,
    ) {
        if !self.is_active() {
            return;
        }
        let Some(cap) = capture else {
            // No stream (a lost device, a suspend): a window cannot hear anything, so it ends.
            self.follow_up = None;
            return;
        };
        cap.wait_for_audio(CAPTURE_POLL);
        match cap.read_f32(usize::MAX) {
            Ok(chunk) if !chunk.is_empty() => {
                if self.follow_up.is_some() {
                    self.pump_follow_up(&chunk, cap, event_tx);
                } else if self.listening {
                    if let Some(diagnostics) = self.diagnostics.as_mut() {
                        diagnostics.observe_audio(&chunk);
                    }
                    let start_sample = self.idle_sample_clock;
                    self.idle_sample_clock = self.idle_sample_clock.saturating_add(chunk.len());
                    self.idle_audio_window.extend(chunk.iter().copied());
                    while self.idle_audio_window.len() > IDLE_AUDIO_WINDOW_SAMPLES {
                        self.idle_audio_window.pop_front();
                        self.idle_audio_origin = self.idle_audio_origin.saturating_add(1);
                    }
                    asr_executor.submit_idle_wake(chunk, start_sample, self.idle_sample_clock);
                } else {
                    self.handoff_audio.extend_from_slice(&chunk);
                    if self.handoff_audio.len() > IDLE_AUDIO_WINDOW_SAMPLES {
                        let excess = self.handoff_audio.len() - IDLE_AUDIO_WINDOW_SAMPLES;
                        self.handoff_audio.drain(..excess);
                    }
                }
            }
            Ok(_) => {}
            Err(error) => {
                tracing::error!(%error, "idle wake capture read failed");
                if let Some(diagnostics) = self.diagnostics.as_mut() {
                    diagnostics.failed("capture_read_error");
                    diagnostics.stopped(self.fire_count);
                }
                self.diagnostics = None;
                self.listening = false;
                self.start_pending = false;
                asr_executor.stop_idle_wake();
                *capture = None;
            }
        }
    }
}
