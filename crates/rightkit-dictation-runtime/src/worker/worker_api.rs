#![allow(unused_imports)]
// Capture coordinator for the sidecar.
//
// Owns one `CaptureSession` and recording buffer. Independent main-ASR and CPU
// KWS workers own their respective models and queues.

pub(crate) use std::fs::OpenOptions;
pub(crate) use std::io::Write;
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender, TryRecvError};
pub(crate) use std::sync::{Arc, OnceLock};
pub(crate) use std::time::{Duration, Instant};

#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{
    capture_recovery::*, probe_decisions::*, transcript_text::*, unified_probe_lane::*,
    worker_clock::*, worker_main::*,
};

pub(crate) use parking_lot::Mutex;
pub(crate) use rightkit_audio::capture::{
    list_input_devices, CaptureFirstBuffer, CaptureSession, CaptureTransport, DeviceInfo,
    MetricsSnapshot, OutDType, SessionConfig,
};

pub(crate) use crate::asr::{
    scheduled_static15_ready_segment, AsrEp, AsrRuntime, ScheduledStatic15,
};
pub(crate) use crate::command_classify::CommandClassification;
pub(crate) use crate::focus::FocusTracker;
pub(crate) use crate::owner_diagnostics;
pub(crate) use rightkit_dictation_core::engine::{FileTranscript, RecordingStageMetrics};
pub(crate) use rightkit_dictation_core::mic_selection::{resolve_mic_selection, MicDeviceSnapshot};
pub(crate) use rightkit_dictation_inference::TranscriptionResult;
pub(crate) use rightkit_text_polish::text_pipeline::{ControlIntent, TimedToken};
pub(crate) use rightkit_voice_commands::command::CommandAction;
pub(crate) use serde_json::json;

pub(crate) const SAMPLE_RATE: u32 = 16_000;
pub(crate) const CAPTURE_POLL: Duration = Duration::from_millis(10);
/// A final dictation must fail closed instead of holding the interaction
/// indefinitely when the main ASR worker wedges.
pub(crate) const FINAL_REPLY_TIMEOUT: Duration = Duration::from_secs(120);
/// File transcription has no delivery authority. It errors only when its
/// decoded-sample progress stalls this long while no dictation holds it back;
/// a long file queued behind dictation never times out.
pub(crate) const FILE_STALL_TIMEOUT: Duration = Duration::from_secs(600);
// Target capture is deferred to each stop boundary. Recording and KWS loops
// never probe AX/UIA for a destination while audio is being collected.

// --- Streaming command / zephyr-tail auto-fire (hands-free, no manual stop) ---
// Silero alone decides whether speech is active and therefore owns the trailing
// pause clock. Perceptual `chunk_level`/`voice_ema` is display-only.
// Silence after speech that gives us one final standalone-command probe. Direct
// catalog commands also probe while speech is still arriving; unfinished direct
// phrases, app-launch verbs, & modifier chords wait until their next word resolves.
pub(crate) const PAUSE_GATE_MS: u64 = 180;
pub(crate) const CMD_RETRY_MS: u64 = 50;
pub(crate) const CMD_PREFIX_GRACE_MS: u64 = 2_500;
pub(crate) const COMMAND_EVENTS_JSONL_MAX_BYTES: u64 = 10 * 1024 * 1024;
// Only the first short utterance can be a standalone command. This is a probe
// bound, not a wait; long dictation should stop paying command-probe cost.
pub(crate) const CMD_WINDOW_SAMPLES: usize = SAMPLE_RATE as usize * 2;
pub(crate) const CMD_MIN_PROBE_SAMPLES: usize = SAMPLE_RATE as usize / 4;
pub(crate) const CMD_NEW_AUDIO_PROBE_SAMPLES: usize = SAMPLE_RATE as usize / 20;
// Once an utterance is longer than the standalone-command window, one main-ASR
// tail check at each speech boundary backs up a confirmed KWS acoustic miss. It
// reuses the strict trailing-control parser without weakening KWS thresholds.
// Keep a small prefix before Silero-confirmed speech so first consonants are not
// trimmed, but measure command eligibility from speech, not from capture start.
pub(crate) const CMD_PREROLL_SAMPLES: usize = SAMPLE_RATE as usize / 4;
// Keep the 20 ms detection cadence, but write routine successful/no-result
// diagnostics only once per second. Candidates, controls, errors, stale results
// & mailbox anomalies remain unsampled.
pub(crate) const KWS_DIAGNOSTIC_SAMPLE_EVERY_PROBES: u64 = 50;
// Expire a bare-Zephyr hint so literal dictated "zephyr" cannot stay armed.
pub(crate) const TAIL_PREFIX_GRACE_MS: u64 = 1_500;
pub(crate) const TAIL_PREFIX_NEW_AUDIO_SAMPLES: usize = SAMPLE_RATE as usize / 20;
pub(crate) const TAIL_WINDOW_SAMPLES: usize = SAMPLE_RATE as usize * 3;
// KWS receives only new suffix audio during normal incremental decoding.
// This two-second snapshot is reset/recovery backfill; native KWS separately
// enforces same maximum decoder-state horizon with overlapping stream epochs.
pub(crate) const FAST_TAIL_WINDOW_SAMPLES: usize = SAMPLE_RATE as usize * 3;
// Initial silence = accidental fire. Once voice has been heard, a long pause is
// end-of-dictation, not a cancellation.
pub(crate) const INITIAL_SILENCE_DISCARD_MS: u64 = 30_000;
pub(crate) const POST_SPEECH_SILENCE_STOP_MS: u64 = 60_000;
// Continuous speech with no pause never trips
// POST_SPEECH_SILENCE_STOP_MS above, so the recording PCM buffer would grow
// unbounded (~230MB/hour at 16kHz f32) and the eventual final decode gets
// increasingly expensive. Hard ceiling regardless of silence state; a
// recording that hits this auto-stops through the SAME mechanism as
// post-speech silence (buffer is transcribed and delivered, not discarded —
// see the `duration_cap_hit` branch in worker/worker_main.rs).
pub(crate) const MAX_RECORDING_SECONDS: u64 = 30 * 60;
pub(crate) const MAX_RECORDING_SAMPLES: usize =
    MAX_RECORDING_SECONDS as usize * SAMPLE_RATE as usize;
pub(crate) const WAKE_DIAGNOSTICS_PREFIX: &str = "RIGHTKIT_WAKE_DIAGNOSTICS_JSON=";
pub(crate) const WAKE_DIAGNOSTICS_INTERVAL: Duration = Duration::from_secs(1);

/// Cross-lane wakeup used while the coordinator is otherwise idle.  The
/// worker cannot select an std mpsc command receiver together with its result
/// receivers, so result producers advance this sequence and unpark the worker.
/// A sequence plus park token closes the check-then-sleep race between command
/// polling and sleeping.
#[derive(Default)]
pub(crate) struct WorkerWaitSignal {
    sequence: std::sync::atomic::AtomicU64,
    waiter: std::sync::OnceLock<std::thread::Thread>,
}

impl WorkerWaitSignal {
    pub(crate) fn snapshot(&self) -> u64 {
        self.sequence.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn notify(&self) {
        self.sequence
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if let Some(waiter) = self.waiter.get() {
            waiter.unpark();
        }
    }

    pub(crate) fn wait_for_change(&self, observed: u64) {
        let _ = self.waiter.set(std::thread::current());
        if self.snapshot() == observed {
            // `unpark` carries a token, so a notify between the sequence
            // check and park cannot be lost.
            std::thread::park();
        }
    }
}

/// Numeric-only idle-wake evidence. It deliberately contains no waveform,
/// transcript, candidate text, device name, or path. The KWS cascade does
/// not expose a confidence score for non-fires, so `score_available` remains
/// false until its scorer API grows one.
#[derive(Debug)]
pub(crate) struct WakeDiagnostics {
    pub(crate) started_at: Instant,
    pub(crate) last_emit: Instant,
    pub(crate) threshold: f32,
    pub(crate) mic_opened: bool,
    pub(crate) audio_samples: usize,
    pub(crate) audio_square_sum: f64,
    pub(crate) audio_peak: f32,
    pub(crate) chunks: u64,
    pub(crate) decode_attempts: u64,
    pub(crate) decode_ms_total: u64,
    pub(crate) decode_ms_max: u64,
    pub(crate) fire_count: u64,
    pub(crate) error_count: u64,
}

impl WakeDiagnostics {
    pub(crate) fn new(threshold: f32, mic_opened: bool) -> Self {
        let now = Instant::now();
        Self {
            started_at: now,
            last_emit: now,
            threshold,
            mic_opened,
            audio_samples: 0,
            audio_square_sum: 0.0,
            audio_peak: 0.0,
            chunks: 0,
            decode_attempts: 0,
            decode_ms_total: 0,
            decode_ms_max: 0,
            fire_count: 0,
            error_count: 0,
        }
    }

    pub(crate) fn started(threshold: f32) -> Self {
        let mut diagnostics = Self::new(threshold, true);
        diagnostics.emit("started", "started", "info", "wake_started");
        diagnostics
    }

    pub(crate) fn startup_failure(threshold: f32, code: &'static str) {
        let mut diagnostics = Self::new(threshold, false);
        diagnostics.error_count = 1;
        diagnostics.emit("startup", "error", "error", code);
    }

    pub(crate) fn observe_audio(&mut self, samples: &[f32]) {
        self.audio_samples = self.audio_samples.saturating_add(samples.len());
        self.chunks = self.chunks.saturating_add(1);
        for &sample in samples {
            let sample = sample as f64;
            self.audio_square_sum += sample * sample;
            self.audio_peak = self.audio_peak.max(sample.abs() as f32);
        }
        self.emit_if_due("observing", "no_match");
    }

    pub(crate) fn observe_decode(&mut self, decode_ms: u64) {
        self.decode_attempts = self.decode_attempts.saturating_add(1);
        self.decode_ms_total = self.decode_ms_total.saturating_add(decode_ms);
        self.decode_ms_max = self.decode_ms_max.max(decode_ms);
        self.emit_if_due("observing", "no_match");
    }

    pub(crate) fn fired(&mut self, fire_count: u64) {
        self.fire_count = fire_count;
        self.emit(
            "threshold_crossed",
            "fired",
            "info",
            "wake_threshold_crossed",
        );
    }

    pub(crate) fn failed(&mut self, stage: &'static str) {
        self.error_count = self.error_count.saturating_add(1);
        self.emit(stage, "error", "error", stage);
    }

    pub(crate) fn stopped(&mut self, fire_count: u64) {
        self.fire_count = fire_count;
        let (severity, code) = self.observation_status();
        self.emit("stopped", "stopped", severity, code);
    }

    pub(crate) fn emit_if_due(&mut self, phase: &'static str, result: &'static str) {
        if self.last_emit.elapsed() >= WAKE_DIAGNOSTICS_INTERVAL {
            let (severity, code) = self.observation_status();
            self.emit(phase, result, severity, code);
        }
    }

    pub(crate) fn observation_status(&self) -> (&'static str, &'static str) {
        if self.error_count > 0 {
            ("error", "wake_stopped_after_error")
        } else if self.audio_samples == 0 && self.started_at.elapsed() >= WAKE_DIAGNOSTICS_INTERVAL
        {
            ("warn", "wake_no_audio")
        } else if self.audio_samples > 0
            && self.decode_attempts == 0
            && self.started_at.elapsed() >= WAKE_DIAGNOSTICS_INTERVAL * 2
        {
            ("warn", "wake_no_kws_decode")
        } else {
            ("info", "wake_observing")
        }
    }

    pub(crate) fn emit(
        &mut self,
        phase: &'static str,
        result: &'static str,
        severity: &'static str,
        code: &'static str,
    ) {
        let payload = self.payload(phase, result, severity, code);
        if let Ok(serialized) = serde_json::to_string(&payload) {
            eprintln!("{WAKE_DIAGNOSTICS_PREFIX}{serialized}");
        }
        self.last_emit = Instant::now();
    }

    pub(crate) fn payload(
        &self,
        phase: &'static str,
        result: &'static str,
        severity: &'static str,
        code: &'static str,
    ) -> serde_json::Value {
        let audio_rms = if self.audio_samples == 0 {
            0.0
        } else {
            (self.audio_square_sum / self.audio_samples as f64).sqrt()
        };
        json!({
            "schema_version": 1,
            "event": "wake_diagnostic",
            "severity": severity,
            "code": code,
            "phase": phase,
            "result": result,
            "elapsed_ms": self.started_at.elapsed().as_millis() as u64,
            "threshold": self.threshold,
            "score_available": false,
            "mic_opened": self.mic_opened,
            "audio_samples": self.audio_samples,
            "audio_rms": audio_rms,
            "audio_peak": self.audio_peak,
            "chunks": self.chunks,
            "kws_decode_attempts": self.decode_attempts,
            "kws_decode_ms_total": self.decode_ms_total,
            "kws_decode_ms_max": self.decode_ms_max,
            "threshold_crossings": self.fire_count,
            "error_count": self.error_count,
        })
    }
}

pub(crate) enum WorkerCapture {
    Live(CaptureSession),
}

impl WorkerCapture {
    pub(crate) fn is_replay(&self) -> bool {
        false
    }

    pub(crate) fn read_f32(&mut self, max_samples: usize) -> Result<Vec<f32>, String> {
        match self {
            Self::Live(capture) => capture
                .read_f32(max_samples)
                .map_err(|error| error.to_string()),
        }
    }

    pub(crate) fn wait_first_buffer(
        &mut self,
        min_samples: usize,
        timeout_ms: u64,
    ) -> Result<CaptureFirstBuffer, String> {
        match self {
            Self::Live(capture) => capture
                .wait_first_buffer(min_samples, timeout_ms)
                .map_err(|error| error.to_string()),
        }
    }

    pub(crate) fn wait_for_audio(&mut self, timeout: Duration) -> bool {
        match self {
            Self::Live(capture) => capture.wait_for_audio(timeout),
        }
    }

    pub(crate) fn flush(&mut self) -> usize {
        match self {
            Self::Live(capture) => capture.flush(),
        }
    }

    pub(crate) fn pause(&self) {
        match self {
            Self::Live(capture) => capture.pause(),
        }
    }

    pub(crate) fn resume(&self) -> Result<(), String> {
        match self {
            Self::Live(capture) => capture.resume().map_err(|error| error.to_string()),
        }
    }

    pub(crate) fn stop(&mut self) -> MetricsSnapshot {
        match self {
            Self::Live(capture) => capture.stop(),
        }
    }

    pub(crate) fn metrics_snapshot(&self) -> MetricsSnapshot {
        match self {
            Self::Live(capture) => capture.metrics_snapshot(),
        }
    }

    pub(crate) fn input_rate(&self) -> u32 {
        match self {
            Self::Live(capture) => capture.input_rate(),
        }
    }

    pub(crate) fn input_channels(&self) -> u16 {
        match self {
            Self::Live(capture) => capture.input_channels(),
        }
    }

    pub(crate) fn input_sample_format(&self) -> &str {
        match self {
            Self::Live(capture) => capture.input_sample_format(),
        }
    }
}

// Command probes are useful operational diagnostics, but the recognized words
// are user content. Keep the event shape/timing while never writing dictated
// text into the ordinary local diagnostic log.
pub(crate) fn command_log_text(_text: &str) -> &'static str {
    "[redacted:diagnostics]"
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SilenceDecision {
    KeepRecording,
    DiscardInitialSilence,
    StopAfterPostSpeechSilence,
}

pub(crate) fn silence_decision(heard_voice: bool, silence_for: Duration) -> SilenceDecision {
    if !heard_voice && silence_for >= Duration::from_millis(INITIAL_SILENCE_DISCARD_MS) {
        return SilenceDecision::DiscardInitialSilence;
    }
    if heard_voice && silence_for >= Duration::from_millis(POST_SPEECH_SILENCE_STOP_MS) {
        return SilenceDecision::StopAfterPostSpeechSilence;
    }
    SilenceDecision::KeepRecording
}

/// Hard duration cap, independent of the heard_voice/silence state
/// `silence_decision` tracks. Checked alongside `StopAfterPostSpeechSilence`
/// at the call site so a recording that hits the cap stops through the exact
/// same transcribe+deliver path as a normal post-speech-silence stop.
pub(crate) fn recording_duration_cap_exceeded(buffer_samples: usize) -> bool {
    buffer_samples >= MAX_RECORDING_SAMPLES
}

#[derive(Debug)]
pub enum WorkerCmd {
    StartWakeListen {
        threshold: f32,
        reply: Sender<Result<(), String>>,
    },
    StopWakeListen {
        reply: Sender<u64>,
    },
    StartRecording {
        session_id: String,
    },
    StopRecording {
        send_enter: bool,
    },
    Cancel,
    /// Reload the ASR model if the configured backend/language changed.
    /// Callers that care about readiness pass a reply channel; recording start
    /// remains independent & waits only before final decode.
    ReloadModel {
        reply: Option<Sender<Result<bool, String>>>,
    },
    TranscribeFile {
        path: PathBuf,
        reply: Sender<Result<FileTranscript, String>>,
    },
    /// Cancellable, progress-reporting file transcription (see
    /// `WorkerHandle::transcribe_file_cancellable`).
    TranscribeFileJob {
        path: PathBuf,
        verbatim: bool,
        cancel: Arc<std::sync::atomic::AtomicBool>,
        progress: Arc<FileProgress>,
        reply: Sender<Result<FileTranscript, String>>,
    },
    /// Reply `Err(code)` to every queued/active file job.
    FailAllFileJobs {
        code: String,
    },
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCutStatus {
    Cut,
    Skipped,
}

/// Typed ownership of why a recording ended. Text is never used to infer a
/// Zephyr control after this boundary; KWS supplies that evidence upstream.
#[derive(Debug, Clone)]
pub enum StopEvidence {
    Manual,
    Silence,
    Duration,
    Zephyr {
        intent: ControlIntent,
        control_start_sample: Option<usize>,
        control_end_sample: Option<usize>,
        cut_status: ControlCutStatus,
    },
    Standalone {
        action: CommandAction,
    },
}

#[derive(Debug, Clone)]
pub struct FinalTranscript {
    pub text: String,
    pub tokens: Vec<TimedToken>,
    pub stop_evidence: StopEvidence,
}

impl FinalTranscript {
    pub fn from_asr(result: TranscriptionResult, stop_evidence: StopEvidence) -> Self {
        Self {
            text: result.text,
            tokens: result
                .tokens
                .into_iter()
                .map(|token| TimedToken {
                    text: token.text,
                    start: token.start,
                    end: token.end,
                })
                .collect(),
            stop_evidence,
        }
    }
}

#[derive(Debug, Clone)]
pub enum WorkerEvent {
    /// The model failed to load; the worker exits after emitting this.
    StartFailed { message: String },
    WakeFired {
        ts: f64,
        fire_count: u64,
        threshold: f32,
    },
    /// Real-time recording amplitude for the pill glow.
    RecordingLevel { session_id: String, level: f32 },
    /// Developer-mode streaming insertion: the full committed (stable) text
    /// so far. Append-only; each event extends the previous one.
    StreamCommitted {
        session_id: String,
        text: String,
        revision: u64,
        audio_secs: f32,
    },
    /// Capture could not become live after one bounded cold reopen.
    CaptureFailed { session_id: String, message: String },
    /// Final transcript (or error) for the active session.
    TranscriptFinal {
        session_id: String,
        result: Result<FinalTranscript, String>,
        capture_warning: Option<String>,
        audio_secs: Option<f32>,
        stage_metrics: RecordingStageMetrics,
    },
    /// The worker detected a standalone command or a `zephyr stop/send/cancel`
    /// tail mid-recording and auto-stopped. `AutoStopBegin` moves the runtime to
    /// Transcribing before final ASR; this result then finalizes it.
    /// For a `zephyr send/stop` control tail the worker has ALREADY stripped the
    /// trigger from `result` using the intent it recognized at fire time (so a
    /// wake-word mis-transcription at delivery can't leave the trigger in the
    /// pasted text); `send_enter` carries the Enter intent that the stripped text
    /// no longer encodes. For a standalone command / silence auto-stop, `result`
    /// carries raw text, timed tokens, and typed stop evidence; the standalone
    /// action is retained in the event instead of reclassified from text.
    AutoStop {
        session_id: String,
        result: Result<FinalTranscript, String>,
        audio_secs: Option<f32>,
        send_enter: bool,
        standalone_action: Option<CommandAction>,
        stage_metrics: RecordingStageMetrics,
    },
    /// The 30s-of-silence backstop tripped — discard the recording (no delivery).
    RunawayDiscard { session_id: String },
    /// A hands-free control tail (`zephyr stop/send`) or standalone command was
    /// recognized and recording has stopped, but the final full-buffer transcribe
    /// or command dispatch is still ahead. Emitted first so the shell can leave
    /// `Recording`, put the PTT hook into release-suppression, and show
    /// "processing" immediately. The `AutoStop` with the transcript follows.
    AutoStopBegin {
        session_id: String,
        send_enter: bool,
        standalone_action: Option<CommandAction>,
    },
    /// A spoken `zephyr cancel` fired mid-recording. Emitted BEFORE the final
    /// full-buffer transcribe so the runtime latches the cancel-to-history
    /// stop kind (`begin_stop(.., local_only=true)`) and the shell leaves
    /// `Recording` immediately. The transcript then follows as a normal
    /// `TranscriptFinal`, whose finalize runs under that latched stop kind:
    /// L0-only polish, no cloud, `cancelled` diagnostics on the wire, and the
    /// shell stores it to history instead of delivering — a cancel must
    /// never discard the user's audio.
    AutoCancelBegin { session_id: String },
}

#[derive(Clone)]
pub struct WorkerHandle {
    pub(crate) cmd_tx: Option<Sender<WorkerCmd>>,
    pub event_rx: Arc<Mutex<Receiver<WorkerEvent>>>,
    pub(crate) cancel_epoch: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) wait_signal: Arc<WorkerWaitSignal>,
    /// File jobs submitted through this handle family and not yet replied.
    pub(crate) file_jobs: Arc<std::sync::atomic::AtomicUsize>,
}

struct FileJobGuard(Arc<std::sync::atomic::AtomicUsize>);

impl FileJobGuard {
    fn new(counter: &Arc<std::sync::atomic::AtomicUsize>) -> Self {
        counter.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Self(Arc::clone(counter))
    }
}

impl Drop for FileJobGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        // Dropping the last public handle disconnects the command receiver;
        // wake an idle worker so it can observe that terminal state.
        let _ = self.cmd_tx.take();
        self.wait_signal.notify();
    }
}

impl WorkerHandle {
    pub fn send(&self, cmd: WorkerCmd) -> Result<(), String> {
        if matches!(&cmd, WorkerCmd::Cancel | WorkerCmd::Shutdown) {
            self.cancel_epoch
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        let result = self
            .cmd_tx
            .as_ref()
            .ok_or_else(|| "worker command channel disconnected".to_string())
            .and_then(|cmd_tx| {
                cmd_tx
                    .send(cmd)
                    .map_err(|_| "worker command channel disconnected".to_string())
            });
        self.wait_signal.notify();
        result
    }

    pub fn reload_model(&self) -> Result<bool, String> {
        let (reply_tx, reply_rx) = channel();
        self.send(WorkerCmd::ReloadModel {
            reply: Some(reply_tx),
        })?;
        reply_rx
            .recv_timeout(Duration::from_secs(30))
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => "worker reload timed out".to_string(),
                RecvTimeoutError::Disconnected => "worker reload reply dropped".to_string(),
            })?
    }

    pub fn start_wake_listen(&self, threshold: f32) -> Result<(), String> {
        let (reply, result) = channel();
        self.send(WorkerCmd::StartWakeListen { threshold, reply })?;
        result
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| "worker wake start timed out".to_string())?
    }

    pub fn stop_wake_listen(&self) -> Result<u64, String> {
        let (reply, result) = channel();
        self.send(WorkerCmd::StopWakeListen { reply })?;
        result
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "worker wake stop timed out".to_string())
    }

    /// Run a file transcription on the worker thread and wait for the result.
    /// Callable on a cloned handle so the IPC layer never holds the
    /// `EngineRuntime` mutex across this (multi-second) round-trip.
    pub fn transcribe_file(&self, path: PathBuf) -> Result<FileTranscript, String> {
        self.transcribe_file_cancellable(path, Arc::new(std::sync::atomic::AtomicBool::new(false)))
    }

    /// Like `transcribe_file`, but the caller may set `cancel` (e.g. the client
    /// disconnected) to drop the job with `Err("file transcription cancelled")`.
    /// Dictation always has priority; the job runs one window at a time in the
    /// gaps and times out only after 10 minutes without progress.
    pub fn transcribe_file_cancellable(
        &self,
        path: PathBuf,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<FileTranscript, String> {
        self.transcribe_file_options(path, cancel, Arc::new(FileProgress::default()), false)
    }

    /// The app's own file transcription (the shell's stdio request). Same as
    /// `transcribe_file_cancellable`, but the job queues ahead of speech-service peers' jobs, so a
    /// sibling app's long file cannot starve the app's own request.
    pub fn transcribe_file_own(
        &self,
        path: PathBuf,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<FileTranscript, String> {
        let progress = Arc::new(FileProgress {
            own: true,
            ..FileProgress::default()
        });
        self.transcribe_file_options(path, cancel, progress, false)
    }

    pub fn transcribe_file_options(
        &self,
        path: PathBuf,
        cancel: Arc<std::sync::atomic::AtomicBool>,
        progress: Arc<FileProgress>,
        verbatim: bool,
    ) -> Result<FileTranscript, String> {
        let _guard = FileJobGuard::new(&self.file_jobs);
        let (reply_tx, reply_rx) = channel();
        self.send(WorkerCmd::TranscribeFileJob {
            path,
            verbatim,
            cancel: Arc::clone(&cancel),
            progress: Arc::clone(&progress),
            reply: reply_tx,
        })?;
        wait_for_file_reply(&reply_rx, &cancel, &progress, FILE_STALL_TIMEOUT)
    }

    /// Fail every queued/active file job with `Err(code)` at the next step
    /// boundary (e.g. engine shutdown or model change).
    pub fn fail_all_file_jobs(&self, code: &str) {
        let _ = self.send(WorkerCmd::FailAllFileJobs {
            code: code.to_string(),
        });
    }

    /// True while any file job submitted through this handle is unfinished.
    pub fn has_file_work(&self) -> bool {
        self.file_jobs.load(std::sync::atomic::Ordering::Acquire) > 0
    }
}

/// Spawn the worker thread. The model is loaded synchronously in the new
/// thread; if it fails, a `StartFailed` event is emitted and the thread exits.
pub fn spawn_worker(
    models_dir: PathBuf,
    ep: AsrEp,
    focus: Arc<Mutex<FocusTracker>>,
) -> Result<WorkerHandle, String> {
    spawn_worker_with_clock(
        models_dir,
        ep,
        focus,
        Arc::new(worker_clock::MonotonicWorkerClock::new()),
    )
}

pub(crate) fn spawn_worker_with_clock(
    models_dir: PathBuf,
    ep: AsrEp,
    focus: Arc<Mutex<FocusTracker>>,
    decision_clock: Arc<dyn worker_clock::WorkerClock>,
) -> Result<WorkerHandle, String> {
    let (cmd_tx, cmd_rx) = channel::<WorkerCmd>();
    let (event_tx, event_rx) = channel::<WorkerEvent>();
    let (ready_tx, ready_rx) = channel::<Result<(), String>>();
    let cancel_epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let wait_signal = Arc::new(WorkerWaitSignal::default());
    let worker_wait_signal = Arc::clone(&wait_signal);
    let worker_cancel_epoch = Arc::clone(&cancel_epoch);

    std::thread::Builder::new()
        .name("hr-engine-worker".to_string())
        .spawn(move || {
            worker_main(
                models_dir,
                ep,
                focus,
                cmd_rx,
                event_tx,
                ready_tx,
                decision_clock,
                worker_cancel_epoch,
                worker_wait_signal,
            )
        })
        .map_err(|e| format!("spawn worker: {e}"))?;

    match ready_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(message)) => return Err(message),
        Err(_) => return Err("worker exited before reporting ready".to_string()),
    }

    Ok(WorkerHandle {
        cmd_tx: Some(cmd_tx),
        event_rx: Arc::new(Mutex::new(event_rx)),
        cancel_epoch,
        wait_signal,
        file_jobs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    })
}

/// Spawn a worker without actually loading a model or touching capture.
/// Used by tests that exercise the runtime state machine without real ASR.
pub fn spawn_dummy_worker(focus: Arc<Mutex<FocusTracker>>) -> Result<WorkerHandle, String> {
    let (cmd_tx, cmd_rx) = channel::<WorkerCmd>();
    let (event_tx, event_rx) = channel::<WorkerEvent>();
    let cancel_epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let wait_signal = Arc::new(WorkerWaitSignal::default());

    std::thread::Builder::new()
        .name("hr-engine-dummy-worker".to_string())
        .spawn(move || {
            // Loop on commands; ignore start/stop; exit only on Shutdown.
            // Holding the focus handle keeps the test reference alive.
            loop {
                match cmd_rx.recv() {
                    Ok(WorkerCmd::Shutdown) | Err(_) => break,
                    Ok(WorkerCmd::StartRecording { .. })
                    | Ok(WorkerCmd::StopRecording { .. })
                    | Ok(WorkerCmd::Cancel) => {}
                    Ok(WorkerCmd::StartWakeListen { reply, .. }) => {
                        let _ = reply.send(Ok(()));
                    }
                    Ok(WorkerCmd::StopWakeListen { reply }) => {
                        let _ = reply.send(0);
                    }
                    Ok(WorkerCmd::ReloadModel { reply }) => {
                        if let Some(reply) = reply {
                            let _ = reply.send(Ok(false));
                        }
                    }
                    Ok(WorkerCmd::TranscribeFile { reply, .. })
                    | Ok(WorkerCmd::TranscribeFileJob { reply, .. }) => {
                        let _ = reply.send(Ok(FileTranscript {
                            text: String::new(),
                            srt: String::new(),
                            vtt: String::new(),
                            words: Vec::new(),
                        }));
                    }
                    Ok(WorkerCmd::FailAllFileJobs { .. }) => {}
                }
            }
            let _ = event_tx;
            let _ = focus;
        })
        .map_err(|e| format!("spawn dummy worker: {e}"))?;

    Ok(WorkerHandle {
        cmd_tx: Some(cmd_tx),
        event_rx: Arc::new(Mutex::new(event_rx)),
        cancel_epoch,
        wait_signal,
        file_jobs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    })
}

/// Reload the ASR model iff the configured backend/language changed since it
/// was last loaded, then WARM it (one throwaway inference) so the first real
/// recording doesn't eat the CoreML graph-compile cost. Returns Ok(true) if it
/// reloaded, Ok(false) if no change. On error the caller keeps the existing
/// model.
pub(crate) fn reload_asr_if_changed(
    model: &mut AsrRuntime,
    last_model_key: &mut String,
    models_dir: &Path,
    ep: AsrEp,
) -> Result<bool, String> {
    let cur_model_key = asr_reload_key();
    if cur_model_key == *last_model_key {
        return Ok(false);
    }
    let warm = Instant::now();
    let mut m = AsrRuntime::load(models_dir, ep)?;
    warm_main_asr(&mut m).map_err(|error| format!("ASR model warmup failed: {error}"))?;
    *model = m;
    *last_model_key = cur_model_key;
    tracing::info!(
        "worker ASR model reloaded+warmed for key={} in {} ms",
        last_model_key,
        warm.elapsed().as_millis()
    );
    Ok(true)
}

/// Probe model is one fixed bundled KWS payload on macOS & Windows,
/// deliberately outside backend/language settings reloads.
pub(crate) fn reload_probe_asr_if_changed(
    model: &mut AsrRuntime,
    last_model_key: &mut String,
    models_dir: &Path,
    ep: AsrEp,
) -> Result<bool, String> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let _ = (model, last_model_key, models_dir, ep);
        Ok(false)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    reload_asr_if_changed(model, last_model_key, models_dir, ep)
}

pub(crate) fn asr_reload_key() -> String {
    format!(
        "{}|{}",
        crate::settings::asr_backend(),
        crate::settings::dictation_language()
    )
}
