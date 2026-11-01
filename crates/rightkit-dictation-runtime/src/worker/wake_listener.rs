#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{unified_probe_lane::*, worker_api::*};

const IDLE_AUDIO_WINDOW_SAMPLES: usize = SAMPLE_RATE as usize * 4;

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
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.listening || self.start_pending
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
                        let handoff_start = end_sample.saturating_add(SAMPLE_RATE as usize / 25);
                        let offset = handoff_start.saturating_sub(self.idle_audio_origin);
                        self.handoff_audio = self
                            .idle_audio_window
                            .iter()
                            .skip(offset)
                            .copied()
                            .collect();
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

    /// Feed idle KWS while listening, or buffer handoff audio while the
    /// runtime has not yet sent StartRecording.
    pub(crate) fn pump_audio(
        &mut self,
        asr_executor: &AsrExecutor,
        capture: &mut Option<WorkerCapture>,
    ) {
        if !self.is_active() {
            return;
        }
        let Some(cap) = capture else {
            return;
        };
        cap.wait_for_audio(CAPTURE_POLL);
        match cap.read_f32(usize::MAX) {
            Ok(chunk) if !chunk.is_empty() => {
                if self.listening {
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
