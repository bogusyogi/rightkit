#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{
    capture_recovery::*, probe_decisions::*, transcript_text::*, worker_api::*, worker_clock::*,
    worker_main::*,
};

#[derive(Debug)]
pub(crate) struct ProbeRequest {
    pub(crate) generation: u64,
    /// Scheduler-owned identity for one native KWS acoustic stream. Recording
    /// generation alone cannot distinguish an old in-flight decode after a
    /// same-recording reset/recovery snapshot.
    pub(crate) kws_stream_generation: u64,
    pub(crate) session_id: String,
    /// Worker decision time at submission. Replay supplies virtual time here;
    /// wall time below remains diagnostic-only mailbox timing.
    pub(crate) submitted_at: worker_clock::WorkerInstant,
    pub(crate) created_at: Instant,
    pub(crate) audio: Vec<f32>,
    pub(crate) speech_start_sample: Option<usize>,
    pub(crate) command_start: usize,
    pub(crate) stream_base_sample: usize,
    /// Absolute first sample accepted by native KWS for this stream. The
    /// worker verifies this at reset time & returns actual value with result.
    pub(crate) native_origin_sample: usize,
    pub(crate) total_samples: usize,
    pub(crate) pending_control_prefix: bool,
    pub(crate) lane: &'static str,
    pub(crate) probe_sequence: u64,
    pub(crate) diagnostic_sample: bool,
}

#[derive(Debug)]
pub(crate) struct ProbeResult {
    pub(crate) request: ProbeRequest,
    pub(crate) transcript: Result<String, String>,
    pub(crate) probe_ms: u64,
    /// Time spent waiting in the single-slot mailbox before inference began.
    /// This is diagnostics only; worker freshness uses submitted_at instead.
    pub(crate) queue_wait_ms: u64,
    /// Monotonic send boundary used to separate result-consumer delay from
    /// mailbox/decode time in stale-result diagnostics.
    pub(crate) completed_at: Instant,
    #[cfg(test)]
    pub(crate) measured_decode_duration: Duration,
    pub(crate) native_origin_sample: usize,
    /// KWS graph output is a closed, canonical command set. Preserve that
    /// typed decision instead of reclassifying its text through fuzzy ASR.
    pub(crate) native_control_intent: Option<rightkit_text_polish::text_pipeline::ControlIntent>,
    /// Absolute sample index into the FULL recording buffer where the trigger
    /// word ("zephyr" or a fuzzy near-homophone — see
    /// `rightkit_text_polish::text_pipeline::has_trailing_control_wake`) was heard
    /// to start, if this probe's decode carried timestamps and a trigger-shaped
    /// word was found in them. `None` when no cut should be attempted; see
    /// `trigger_onset_skip_reason` for why.
    pub(crate) trigger_onset_sample: Option<usize>,
    /// Set whenever `trigger_onset_sample` is `None`, so the consumer can log
    /// WHY the audio cut is being skipped without recomputing anything:
    /// `"no_timestamps"` (this ASR backend/decode did not produce timed
    /// pieces) or `"piece_not_found"` (timed pieces decoded fine, but nothing
    /// in them looked like the trigger word). The consumer additionally
    /// derives an `"implausible"` fallback reason itself, since that check
    /// needs the CURRENT buffer/speech-start state at consumption time, not
    /// what was true when this probe was decoded.
    pub(crate) trigger_onset_skip_reason: Option<&'static str>,
}

#[derive(Debug)]
pub(crate) struct IdleWakeRequest {
    pub(crate) audio: Vec<f32>,
    pub(crate) start_sample: usize,
    pub(crate) total_samples: usize,
}

#[derive(Debug)]
pub(crate) struct IdleWakeResult {
    pub(crate) hit: Result<Option<(usize, usize)>, String>,
    pub(crate) total_samples: usize,
    pub(crate) decode_ms: u64,
}

#[derive(Debug)]
pub(crate) struct OpeningCommandRequest {
    pub(crate) generation: u64,
    pub(crate) session_id: String,
    #[cfg(test)]
    pub(crate) submitted_at: worker_clock::WorkerInstant,
    pub(crate) created_at: Instant,
    pub(crate) audio: Vec<f32>,
    pub(crate) speech_start_sample: Option<usize>,
    pub(crate) command_start: usize,
    pub(crate) total_samples: usize,
    pub(crate) pause_ready: bool,
    pub(crate) pending_prefix: bool,
}

#[derive(Debug)]
pub(crate) struct OpeningCommandResult {
    pub(crate) request: OpeningCommandRequest,
    pub(crate) transcript: Result<String, String>,
    pub(crate) decode_ms: u64,
    #[cfg(test)]
    pub(crate) measured_decode_duration: Duration,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LaneCompletion {
    pub(crate) lane: worker_clock::ExecutorLane,
    pub(crate) submitted_at: worker_clock::WorkerInstant,
    pub(crate) measured_decode_duration: Duration,
}

pub(crate) fn should_use_fresh_confirmation_fallback(lane: &str, pending_prefix: bool) -> bool {
    pending_prefix && lane == "zephyr_tail_full_async"
}

/// Cleanup-first is limited to a confirmed native KWS stop/send. Text-only
/// fallback controls and manual stops retain their established audio-cut path.
pub(crate) fn should_use_cleanup_first_control(
    model_supported: bool,
    intent: rightkit_text_polish::text_pipeline::ControlIntent,
    native_intent: Option<rightkit_text_polish::text_pipeline::ControlIntent>,
    onset_sample: Option<usize>,
) -> bool {
    model_supported
        && matches!(
            intent,
            rightkit_text_polish::text_pipeline::ControlIntent::Stop
                | rightkit_text_polish::text_pipeline::ControlIntent::Send
        )
        && native_intent == Some(intent)
        && onset_sample.is_some_and(|onset| onset > 0)
}

#[cfg(test)]
impl LaneCompletion {
    pub(crate) fn schedule_on(self, schedule: &mut worker_clock::ReplayEventSchedule) {
        schedule.schedule_lane_completion(
            self.lane,
            self.submitted_at,
            self.measured_decode_duration,
        );
    }
}

#[cfg(test)]
pub(crate) trait HasLaneCompletion {
    fn lane_completion(&self) -> LaneCompletion;
}

#[cfg(test)]
impl HasLaneCompletion for ProbeResult {
    fn lane_completion(&self) -> LaneCompletion {
        LaneCompletion {
            lane: worker_clock::ExecutorLane::Kws,
            submitted_at: self.request.submitted_at,
            measured_decode_duration: self.measured_decode_duration,
        }
    }
}

#[cfg(test)]
impl HasLaneCompletion for OpeningCommandResult {
    fn lane_completion(&self) -> LaneCompletion {
        LaneCompletion {
            lane: worker_clock::ExecutorLane::MainAsr,
            submitted_at: self.request.submitted_at,
            measured_decode_duration: self.measured_decode_duration,
        }
    }
}

/// Keep bounded VAD history for recording lifecycle bookkeeping. Terminal
/// control cutting uses Static15 RMS seams, not this tracker.
pub(crate) const TERMINAL_SILENCE_HISTORY_SAMPLES: usize = SAMPLE_RATE as usize * 4;
pub(crate) const VAD_FRAME_SAMPLES: usize = 512;

#[derive(Debug, Default)]
pub(crate) struct TerminalSilenceTracker {
    /// First unconsumed PCM sample. It mirrors SpeechVad's 512-sample pending
    /// queue, including its loading phase, so delayed VAD readiness cannot
    /// misalign a silence verdict with capture PCM.
    pub(crate) pending_start_sample: Option<usize>,
    pub(crate) pending_end_sample: usize,
    pub(crate) active_silence: Option<std::ops::Range<usize>>,
    pub(crate) completed_silences: Vec<std::ops::Range<usize>>,
    /// The current silence run already crossed `KEEP_WARM_BURST_SILENCE_SAMPLES`;
    /// cleared by the next speech frame.
    pub(crate) keep_warm_burst_fired: bool,
}

/// Silence after which a recording dictation gets a keep-warm burst: the
/// speaker may be about to stop, so the GPU is warmed before the final pass.
pub(crate) const KEEP_WARM_BURST_SILENCE_SAMPLES: usize = SAMPLE_RATE as usize * 300 / 1000;

impl TerminalSilenceTracker {
    pub(crate) fn reset(&mut self) {
        self.pending_start_sample = None;
        self.pending_end_sample = 0;
        self.active_silence = None;
        self.completed_silences.clear();
        self.keep_warm_burst_fired = false;
    }

    /// `None` is VAD unavailable/unknown, never silence. Each boolean is one
    /// contiguous 32 ms VAD frame in capture order. Returns true once per
    /// silence run, when it first reaches `KEEP_WARM_BURST_SILENCE_SAMPLES`.
    pub(crate) fn observe(
        &mut self,
        chunk_start_sample: usize,
        chunk_len: usize,
        verdicts: Option<&[bool]>,
    ) -> bool {
        if chunk_len == 0 {
            return false;
        }
        let chunk_end = chunk_start_sample.saturating_add(chunk_len);
        if self
            .pending_start_sample
            .is_some_and(|_| self.pending_end_sample != chunk_start_sample)
        {
            // A capture discontinuity means we cannot map pending VAD frames
            // to PCM safely. Discard only this terminal-boundary evidence.
            self.reset();
        }
        self.pending_start_sample.get_or_insert(chunk_start_sample);
        self.pending_end_sample = chunk_end;

        let Some(verdicts) = verdicts else {
            return false;
        };
        let mut burst = false;
        for speech in verdicts {
            let Some(frame_start) = self.pending_start_sample else {
                break;
            };
            let frame_end = frame_start.saturating_add(VAD_FRAME_SAMPLES);
            if frame_end > self.pending_end_sample {
                // Never guess alignment if VAD/backend bookkeeping differs.
                self.reset();
                return false;
            }
            self.pending_start_sample = Some(frame_end);
            if *speech {
                self.keep_warm_burst_fired = false;
                if let Some(silence) = self.active_silence.take() {
                    self.completed_silences.push(silence);
                }
            } else if let Some(silence) = self.active_silence.as_mut() {
                silence.end = frame_end;
            } else {
                self.active_silence = Some(frame_start..frame_end);
            }
            if !self.keep_warm_burst_fired
                && self
                    .active_silence
                    .as_ref()
                    .is_some_and(|s| s.len() >= KEEP_WARM_BURST_SILENCE_SAMPLES)
            {
                self.keep_warm_burst_fired = true;
                burst = true;
            }
        }

        let keep_from = chunk_end.saturating_sub(TERMINAL_SILENCE_HISTORY_SAMPLES);
        self.completed_silences
            .retain(|silence| silence.end > keep_from);
        burst
    }
}

/// Reconstructs word boundaries from piece-level ASR timestamps (SentencePiece
/// `▁` convention: a piece whose display text starts with whitespace opens a
/// new word — both the CoreML and ONNX Parakeet decoders already follow this
/// convention, see
/// `rightkit-dictation-inference/src/coreml_asr/engine.rs::transcribe_pieces_timed`
/// and `parakeet-rs`'s `decoder_tdt.rs::decode_with_timestamps`) and returns
/// the ABSOLUTE sample index — into the FULL recording buffer, not just this
/// probe's audio slice — where the LAST trigger-shaped word begins. Taking the
/// LAST match (rather than the first) matches the control grammar, which
/// anchors the wake word immediately before the verb at the END of the
/// utterance.
pub(crate) fn find_trigger_onset_sample(
    stream_base_sample: usize,
    tokens: &[rightkit_dictation_inference::TimedToken],
) -> Option<usize> {
    let mut best_start_sec: Option<f32> = None;
    let mut current_word = String::new();
    let mut current_start_sec: f32 = 0.0;
    let mut has_word = false;

    for token in tokens {
        let starts_new_word = token.text.chars().next().is_some_and(char::is_whitespace);
        if starts_new_word && has_word {
            if word_looks_like_trigger(&current_word) {
                best_start_sec = Some(current_start_sec);
            }
            current_word.clear();
            has_word = false;
        }
        if !has_word {
            current_start_sec = token.start;
            has_word = true;
        }
        current_word.push_str(&token.text);
    }
    if has_word && word_looks_like_trigger(&current_word) {
        best_start_sec = Some(current_start_sec);
    }

    let onset_sec = best_start_sec?;
    let onset_samples_in_slice = (onset_sec.max(0.0) * SAMPLE_RATE as f32).round() as usize;
    Some(stream_base_sample.saturating_add(onset_samples_in_slice))
}

pub(crate) fn resolve_trigger_onset_sample(
    explicit_wake_start: Option<usize>,
    stream_base_sample: usize,
    tokens: &[rightkit_dictation_inference::TimedToken],
) -> Option<usize> {
    explicit_wake_start.or_else(|| find_trigger_onset_sample(stream_base_sample, tokens))
}

pub(crate) fn word_looks_like_trigger(word: &str) -> bool {
    let trimmed = word.trim();
    !trimmed.is_empty() && rightkit_text_polish::text_pipeline::has_trailing_control_wake(trimmed)
}

pub(crate) fn native_control_intent(
    text: &str,
) -> Option<rightkit_text_polish::text_pipeline::ControlIntent> {
    use rightkit_text_polish::text_pipeline::ControlIntent;
    match text.trim().to_ascii_lowercase().as_str() {
        "zephyr stop" => Some(ControlIntent::Stop),
        "zephyr send" => Some(ControlIntent::Send),
        "zephyr cancel" => Some(ControlIntent::Cancel),
        _ => None,
    }
}

pub(crate) fn incremental_probe_offset(
    processed_total: usize,
    snapshot_start: usize,
    snapshot_len: usize,
) -> Option<usize> {
    processed_total
        .checked_sub(snapshot_start)
        .map(|offset| offset.min(snapshot_len))
}

pub(crate) fn probe_reset_stream_base(
    requested_stream_base: usize,
    snapshot_start: usize,
    incremental_offset: Option<usize>,
) -> usize {
    if incremental_offset.is_some() {
        requested_stream_base
    } else {
        snapshot_start
    }
}

/// CPU KWS transcribe with piece timestamps so trigger onset can be located
/// without a second decode pass. KWS owns a continuous incremental stream.
/// Batch `condition_for_asr` cannot be applied to each delta independently —
/// its DC removal, HPF state, and utterance-level gain decision restart every
/// 20 ms, producing discontinuities at accept boundaries. Feed KWS raw
/// PCM, matching production replay. A stateful streaming conditioner would
/// be legal, but 2026-10-01 saved-session replays measured no wake-recall
/// gain from it: the observed first-command miss is encoder chunk-grid phase
/// lock, not audio level. Other probe backends keep normal ASR conditioning.
pub(crate) fn transcribe_probe_buffer_timed(
    model: &mut AsrRuntime,
    buffer: &[f32],
) -> Result<TranscriptionResult, String> {
    if model.requires_timed_control_probe() {
        return model.transcribe_probe_result(buffer);
    }
    let audio_policy = rightkit_dictation_core::dev_var!("RIGHTKIT_AUDIO_CONDITIONING_POLICY")
        .unwrap_or_else(|_| "default".into());
    let conditioned = rightkit_dictation_core::audio_conditioning::condition_for_asr(
        buffer,
        SAMPLE_RATE,
        &audio_policy,
    );
    model.transcribe_probe_result(&conditioned)
}

pub(crate) fn trace_trigger_audio_cut(
    session_id: &str,
    onset_sample: usize,
    cut_at: usize,
    cut_source: &'static str,
    seam_rms: Option<f32>,
    noise_floor_rms: Option<f32>,
    buffer_len: usize,
) {
    tracing::info!(
        onset_sample,
        cut_at,
        cut_source,
        ?seam_rms,
        ?noise_floor_rms,
        buffer_len,
        "trigger_audio_cut"
    );
    emit_command_probe(json!({
        "event": "trigger_audio_cut",
        "session_id": session_id,
        "onset_sample": onset_sample,
        "cut_at": cut_at,
        "cut_source": cut_source,
        "seam_rms": seam_rms,
        "noise_floor_rms": noise_floor_rms,
        "buffer_len": buffer_len,
    }));
}

pub(crate) fn trace_trigger_audio_cut_fallback(
    session_id: Option<&str>,
    reason: &'static str,
    buffer_len: usize,
) {
    tracing::info!(reason, buffer_len, "trigger_audio_cut_fallback");
    emit_command_probe(json!({
        "event": "trigger_audio_cut_fallback",
        "session_id": session_id,
        "reason": reason,
        "buffer_len": buffer_len,
    }));
}

pub(crate) enum MainAsrControl {
    Reload {
        reply: Sender<Result<(bool, bool, String), String>>,
    },
    Cancel {
        generation: u64,
    },
    Shutdown,
}

enum ReloadCoordinatorCommand {
    Request { id: u64 },
    Shutdown,
}

pub(crate) struct ReloadCompletion {
    pub(crate) id: u64,
    pub(crate) result: Result<bool, String>,
}

pub(crate) enum FinalRequest {
    Dictation {
        generation: u64,
        audio: Vec<f32>,
        reply: Sender<Result<TranscriptionResult, String>>,
    },
    ZephyrControl {
        generation: u64,
        audio: Vec<f32>,
        onset_sample: usize,
        intent: rightkit_text_polish::text_pipeline::ControlIntent,
        reply: Sender<Result<FinalControlResult, String>>,
    },
}

/// Final transcription plus cleanup evidence for a confirmed stop/send.
/// Unsupported runtimes return `method = "legacy_cut"` and no boundary.
#[derive(Debug)]
pub(crate) struct FinalControlResult {
    pub(crate) original_text: String,
    pub(crate) transcript: TranscriptionResult,
    pub(crate) method: &'static str,
    pub(crate) boundary_sec: Option<f32>,
    pub(crate) score: Option<f32>,
    pub(crate) joint_calls: usize,
    pub(crate) error: Option<String>,
}

pub(crate) struct BackgroundRequest {
    pub(crate) generation: u64,
    pub(crate) start_sample: usize,
    pub(crate) end_sample: usize,
    pub(crate) audio: Vec<f32>,
}

/// Streaming insertion (Developer mode): one decode of the trailing window.
/// Latest-wins; a newer request replaces one not yet started.
pub(crate) struct StreamRequest {
    pub(crate) generation: u64,
    pub(crate) start_sample: usize,
    pub(crate) audio: Vec<f32>,
}

pub(crate) struct StreamResult {
    pub(crate) generation: u64,
    pub(crate) start_sample: usize,
    pub(crate) end_sample: usize,
    pub(crate) result: Result<TranscriptionResult, String>,
}

/// Commit only contiguous background windows. If one fails, caller retains it
/// for retry before advancing, so final decode never receives a silent gap.
pub(crate) fn commit_background_window<F>(
    scheduled: &mut ScheduledStatic15,
    request: &BackgroundRequest,
    decode: F,
) -> Result<String, String>
where
    F: FnOnce(&[f32]) -> Result<TranscriptionResult, String>,
{
    let result = decode(&request.audio)?;
    scheduled.commit_window(
        request.start_sample,
        request.end_sample,
        &result.text,
        result.tokens,
    )?;
    Ok(result.text)
}

pub(crate) enum MainAsrJob {
    LeaseYield,
    Final(FinalRequest),
    Control(MainAsrControl),
    Opening(OpeningCommandRequest),
    Background(BackgroundRequest),
    Stream(StreamRequest),
    /// One ~15 s window of a queued file job. Lowest priority.
    File(FileWork),
    /// Windows/DirectML only: a short decode while a dictation is recording and
    /// the model has been idle for `KEEP_WARM_IDLE`. A discrete GPU drops its
    /// clocks after ~1.5 s idle, and the first pass after that pays ~230 ms
    /// (final-tail encoder 51-64 ms when the GPU was busy <1.5 s earlier,
    /// 228-315 ms after >3 s idle; docs/latency/2026-10-23-mac-vs-windows-stop-to-paste.md).
    KeepWarm,
}

/// Idle time after which a recording dictation gets a keep-warm decode.
/// The RTX starts clocking down within 1 s and is parked by 2-3 s; 0.5 s beat 0.8 s and
/// matched 0.3 s within noise at lower power (keep-warm sweep 2026-10-23).
pub(crate) const KEEP_WARM_IDLE: Duration = Duration::from_millis(500);

/// Keep-warm runs on Windows (DirectML GPU) unless the dev switch turns it off, on battery too:
/// it only runs while a dictation is recording, and skipping it costs the final pass the GPU
/// wake-up (228-315 ms after idle; keep-warm sweep 2026-10-23).
pub(crate) fn keep_warm_enabled() -> bool {
    cfg!(target_os = "windows")
        && rightkit_dictation_core::dev_var!("RIGHTKIT_ASR_KEEP_WARM_DISABLED")
            .map_or(true, |v| v.trim() != "1")
}

/// Shared progress for one file job. `done` advances after each decoded
/// window; `paused` is true while the job is waiting behind dictation, so the
/// caller's stall clock does not run during a long recording.
#[derive(Debug, Default)]
pub struct FileProgress {
    pub done: std::sync::atomic::AtomicUsize,
    pub total: std::sync::atomic::AtomicUsize,
    pub started: std::sync::atomic::AtomicBool,
    pub paused: std::sync::atomic::AtomicBool,
    /// The app's own request (the shell's stdio call), not a speech-service peer's. An own job
    /// queues ahead of peer jobs that have not started (`MainAsrMailbox::push_file`).
    pub own: bool,
}

/// A file job queued behind dictation. The worker steps it one window at a
/// time; the job owns the (already conditioned) samples.
pub(crate) struct FileWork {
    pub(crate) job: crate::asr::FileJob,
    pub(crate) reply: Sender<Result<FileTranscript, String>>,
    pub(crate) cancel: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) progress: Arc<FileProgress>,
    /// Fail-all epoch observed when the request was created. `None` is
    /// stamped with the mailbox's current epoch on push.
    pub(crate) epoch: Option<u64>,
}

impl FileWork {
    pub(crate) fn new(
        samples: Vec<f32>,
        reply: Sender<Result<FileTranscript, String>>,
        cancel: Arc<std::sync::atomic::AtomicBool>,
        progress: Arc<FileProgress>,
    ) -> Self {
        Self {
            job: crate::asr::FileJob::new(samples),
            reply,
            cancel,
            progress,
            epoch: None,
        }
    }
}

pub(crate) const FILE_CANCELLED: &str = "file transcription cancelled";
pub(crate) const FILE_RELOADED: &str = "speech model reloaded; retry";
const FILE_WAIT_POLL: Duration = Duration::from_millis(100);

/// Wait for a file job reply. Errors only when the job makes no progress for
/// `stall` while no dictation holds it back, or when the caller cancels.
pub(crate) fn wait_for_file_reply(
    reply: &Receiver<Result<FileTranscript, String>>,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &FileProgress,
    stall: Duration,
) -> Result<FileTranscript, String> {
    use std::sync::atomic::Ordering::{Acquire, Release};
    let mut last_done = progress.done.load(Acquire);
    let mut last_move = Instant::now();
    loop {
        match reply.recv_timeout(FILE_WAIT_POLL) {
            Ok(result) => return result,
            Err(RecvTimeoutError::Disconnected) => {
                return Err("worker transcription reply dropped".to_string());
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if cancel.load(Acquire) {
            return Err(FILE_CANCELLED.to_string());
        }
        let done = progress.done.load(Acquire);
        if done != last_done || progress.paused.load(Acquire) {
            last_done = done;
            last_move = Instant::now();
        } else if last_move.elapsed() >= stall {
            cancel.store(true, Release);
            return Err("file_queue_stall_timeout: no decode progress in 600 s".to_string());
        }
    }
}

/// Cheap clone-able entry for submitting a file job from a helper thread.
#[derive(Clone)]
pub(crate) struct FileSubmitter {
    mailbox: Arc<MainAsrMailbox>,
    main_alive: Arc<std::sync::atomic::AtomicBool>,
    epoch: u64,
}

impl FileSubmitter {
    /// Queue `samples` (already conditioned) and block until the job replies.
    pub(crate) fn run(
        &self,
        samples: Vec<f32>,
        cancel: Arc<std::sync::atomic::AtomicBool>,
        progress: Arc<FileProgress>,
        verbatim: bool,
    ) -> Result<FileTranscript, String> {
        use std::sync::atomic::Ordering::Acquire;
        if !self.main_alive.load(Acquire) {
            return Err("main ASR worker is not running".to_string());
        }
        let (reply_tx, reply_rx) = channel();
        progress
            .total
            .store(samples.len(), std::sync::atomic::Ordering::Release);
        let mut work = FileWork::new(samples, reply_tx, Arc::clone(&cancel), progress);
        work.job = work.job.with_verbatim(verbatim);
        work.epoch = Some(self.epoch);
        self.mailbox.push_file(work);
        loop {
            match reply_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(result) => return result,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("main ASR file reply dropped".to_string());
                }
                Err(RecvTimeoutError::Timeout) => {
                    if !self.main_alive.load(Acquire) {
                        return Err("main ASR worker is not running".to_string());
                    }
                    if cancel.load(Acquire) {
                        return Err(FILE_CANCELLED.to_string());
                    }
                }
            }
        }
    }
}

pub(crate) enum ProbeControl {
    Reload {
        reply: Sender<Result<(bool, String), String>>,
    },
    StartIdleWake {
        threshold: f32,
        reply: Sender<Result<(), String>>,
    },
    StopIdleWake {
        reply: Sender<()>,
    },
    Reset,
    Shutdown,
}

pub(crate) enum ProbeJob {
    Control(ProbeControl),
    Probe(ProbeRequest),
    IdleWake(IdleWakeRequest),
}

#[derive(Default)]
pub(crate) struct MainAsrQueueState {
    pub(crate) final_requests: std::collections::VecDeque<FinalRequest>,
    pub(crate) controls: std::collections::VecDeque<MainAsrControl>,
    pub(crate) latest_opening: Option<OpeningCommandRequest>,
    pub(crate) background_requests: std::collections::VecDeque<BackgroundRequest>,
    pub(crate) latest_stream: Option<StreamRequest>,
    /// Queued file jobs, earliest first. Never cleared by dictation traffic.
    pub(crate) file_works: std::collections::VecDeque<FileWork>,
    /// Bumped by `fail_all_file_jobs`; jobs stamped with an older epoch fail.
    pub(crate) file_fail_epoch: u64,
    pub(crate) file_fail_code: String,
    /// True from dictation start until its final/cancel completes. File
    /// steps never run while this is set.
    pub(crate) dictation_active: bool,
    /// Keep-warm passes still owed by a silence burst; any real work runs first.
    pub(crate) keep_warm_burst: u8,
    /// `RIGHTKIT_ASR_KEEP_WARM_PAUSE_MODE=dense`: back-to-back keep-warm while the speaker pauses.
    /// Always false under the default pause mode.
    pub(crate) keep_warm_continuous: bool,
}

/// Back-to-back keep-warm passes queued when a dictation goes silent.
pub(crate) const KEEP_WARM_BURST_PASSES: u8 = 2;

pub(crate) struct MainAsrMailbox {
    pub(crate) state: Mutex<MainAsrQueueState>,
    pub(crate) ready: parking_lot::Condvar,
    pub(crate) exclusivity: Arc<ProbeLaneExclusivity>,
}

impl Default for MainAsrMailbox {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            ready: parking_lot::Condvar::new(),
            exclusivity: Arc::new(ProbeLaneExclusivity::default()),
        }
    }
}

impl MainAsrMailbox {
    pub(crate) fn submit_opening(&self, request: OpeningCommandRequest) -> bool {
        let mut state = self.state.lock();
        if !self.exclusivity.queue_opening() {
            return false;
        }
        #[cfg(test)]
        replay_mark_lane_in_flight(worker_clock::ExecutorLane::MainAsr, request.submitted_at);
        state.latest_opening = Some(request);
        drop(state);
        self.ready.notify_one();
        true
    }

    pub(crate) fn submit_background(&self, request: BackgroundRequest) {
        self.state.lock().background_requests.push_back(request);
        self.ready.notify_one();
    }

    pub(crate) fn submit_stream(&self, request: StreamRequest) {
        self.state.lock().latest_stream = Some(request);
        self.ready.notify_one();
    }

    pub(crate) fn push_final(&self, final_request: FinalRequest) {
        let mut state = self.state.lock();
        if state.latest_opening.take().is_some() {
            self.exclusivity.clear_opening_queue();
        }
        state.background_requests.clear();
        state.latest_stream = None;
        state.final_requests.push_back(final_request);
        rightkit_dictation_inference::timeline_recorder::mark(|t| &mut t.final_queued_ms);
        drop(state);
        self.ready.notify_one();
    }

    /// Queue a file job. Deliberately does NOT clear opening/background work:
    /// a live dictation's probes must survive file traffic. the app's own jobs go ahead of
    /// queued peer jobs, never ahead of one already stepping.
    pub(crate) fn push_file(&self, mut work: FileWork) {
        let mut state = self.state.lock();
        work.epoch.get_or_insert(state.file_fail_epoch);
        work.progress
            .paused
            .store(state.dictation_active, std::sync::atomic::Ordering::Release);
        if work.progress.own {
            // After any job already stepping and any earlier own job, before the first peer job.
            let index = state
                .file_works
                .iter()
                .position(|queued| {
                    !queued.progress.own
                        && !queued
                            .progress
                            .started
                            .load(std::sync::atomic::Ordering::Acquire)
                })
                .unwrap_or(state.file_works.len());
            state.file_works.insert(index, work);
        } else {
            state.file_works.push_back(work);
        }
        drop(state);
        self.ready.notify_one();
    }

    /// Put a job back after one step; it stays ahead of later submissions.
    pub(crate) fn requeue_file(&self, work: FileWork) {
        let mut state = self.state.lock();
        work.progress
            .paused
            .store(state.dictation_active, std::sync::atomic::Ordering::Release);
        state.file_works.push_front(work);
    }

    /// Reply `code` to every queued job now; a job currently mid-step fails
    /// at its next step boundary through the epoch check.
    pub(crate) fn fail_all_file_jobs(&self, code: &str) {
        let mut state = self.state.lock();
        state.file_fail_epoch = state.file_fail_epoch.wrapping_add(1);
        state.file_fail_code = code.to_string();
        let drained: Vec<FileWork> = state.file_works.drain(..).collect();
        drop(state);
        for work in drained {
            let _ = work.reply.send(Err(code.to_string()));
        }
        self.ready.notify_one();
    }

    /// Failure code for a job taken before the latest `fail_all_file_jobs`.
    pub(crate) fn file_failure_for(&self, work: &FileWork) -> Option<String> {
        let state = self.state.lock();
        match work.epoch {
            Some(epoch) if epoch != state.file_fail_epoch => Some(state.file_fail_code.clone()),
            _ => None,
        }
    }

    pub(crate) fn file_fail_epoch(&self) -> u64 {
        self.state.lock().file_fail_epoch
    }

    pub(crate) fn has_file_work(&self) -> bool {
        !self.state.lock().file_works.is_empty()
    }

    /// Dictation owns the model from start through final/cancel. File steps
    /// pause while set and resume (condvar notify) when it clears.
    pub(crate) fn set_dictation_active(&self, active: bool) {
        let mut state = self.state.lock();
        if state.dictation_active == active {
            return;
        }
        state.dictation_active = active;
        state.keep_warm_burst = 0;
        state.keep_warm_continuous = false;
        for work in state.file_works.iter() {
            work.progress
                .paused
                .store(active, std::sync::atomic::Ordering::Release);
        }
        drop(state);
        // A thread idling untimed (`RIGHTKIT_MAIN_ASR_IDLE_WAIT=untimed`) must see a dictation start
        // for keep-warm; under the default it polls, exactly as before.
        if !active
            || crate::perf_levers::main_asr_idle_wait().wake_on_dictation_start(keep_warm_enabled())
        {
            self.ready.notify_one();
        }
    }

    /// `RIGHTKIT_ASR_KEEP_WARM_PAUSE_MODE=dense`: start or stop back-to-back keep-warm passes for a
    /// recording dictation. A no-op when nothing changes or no dictation is active.
    pub(crate) fn set_keep_warm_continuous(&self, on: bool) {
        let mut state = self.state.lock();
        if !state.dictation_active || state.keep_warm_continuous == on {
            return;
        }
        state.keep_warm_continuous = on;
        drop(state);
        if on {
            self.ready.notify_one();
        }
    }

    /// Queue `KEEP_WARM_BURST_PASSES` keep-warm passes for a recording
    /// dictation that just went silent. Replaces, never adds to, a pending burst.
    pub(crate) fn request_keep_warm_burst(&self) {
        let mut state = self.state.lock();
        if !state.dictation_active {
            return;
        }
        state.keep_warm_burst = KEEP_WARM_BURST_PASSES;
        drop(state);
        self.ready.notify_one();
    }

    fn sweep_cancelled_files(state: &mut MainAsrQueueState) {
        if !state
            .file_works
            .iter()
            .any(|work| work.cancel.load(std::sync::atomic::Ordering::Acquire))
        {
            return;
        }
        let queued = std::mem::take(&mut state.file_works);
        for work in queued {
            if work.cancel.load(std::sync::atomic::Ordering::Acquire) {
                let _ = work.reply.send(Err(FILE_CANCELLED.to_string()));
            } else {
                state.file_works.push_back(work);
            }
        }
    }

    pub(crate) fn push_control(&self, control: MainAsrControl) {
        let mut state = self.state.lock();
        if matches!(control, MainAsrControl::Cancel { .. }) {
            if state.latest_opening.take().is_some() {
                self.exclusivity.clear_opening_queue();
            }
            state.background_requests.clear();
        }
        state.controls.push_back(control);
        drop(state);
        self.ready.notify_one();
    }

    pub(crate) fn recv(&self) -> MainAsrJob {
        // recv() is only called once the previous job finished, so the model
        // has been idle since now.
        let idle_since = Instant::now();
        let keep_warm = keep_warm_enabled();
        let keep_warm_idle =
            crate::perf_levers::keep_warm_idle_override().unwrap_or(KEEP_WARM_IDLE);
        let untimed_idle = crate::perf_levers::main_asr_idle_wait().untimed_when_idle(keep_warm);
        let mut state = self.state.lock();
        loop {
            // Release only between whole file jobs, never between their windows.
            let boundary = state.file_works.front().is_none_or(|w| {
                !w.progress
                    .started
                    .load(std::sync::atomic::Ordering::Acquire)
            });
            if !state.dictation_active && boundary && crate::speech_service::lease::should_yield() {
                return MainAsrJob::LeaseYield;
            }
            let needs_model = state
                .controls
                .iter()
                .any(|c| matches!(c, MainAsrControl::Reload { .. }))
                || !state.final_requests.is_empty()
                || state.latest_opening.is_some()
                || !state.background_requests.is_empty()
                || state.latest_stream.is_some()
                || (!state.dictation_active && !state.file_works.is_empty());
            if needs_model
                && !crate::speech_service::lease::try_resume().unwrap_or_else(|error| {
                    tracing::error!(event = "service_lease_resume_failed", %error);
                    false
                })
            {
                if let Some(waited) =
                    crate::speech_service::lease::lease_wait_expired(Instant::now())
                {
                    let error = format!(
                        "model_lease_unavailable: holder unknown; waited {} s after yield request",
                        waited.as_secs()
                    );
                    drop(state);
                    self.fail_all_file_jobs(&error);
                    crate::speech_service::lease::clear_lease_wait();
                    state = self.state.lock();
                }
                // Continue accepting capture/control while a peer owns model.
                if let Some(index) = state
                    .controls
                    .iter()
                    .position(|c| !matches!(c, MainAsrControl::Reload { .. }))
                {
                    return MainAsrJob::Control(
                        state.controls.remove(index).expect("control exists"),
                    );
                }
                self.ready.wait_for(&mut state, Duration::from_millis(250));
                continue;
            }
            if let Some(final_request) = state.final_requests.pop_front() {
                return MainAsrJob::Final(final_request);
            }
            if let Some(control) = state.controls.pop_front() {
                return MainAsrJob::Control(control);
            }
            if let Some(request) = state.latest_opening.take() {
                self.exclusivity.opening_started();
                return MainAsrJob::Opening(request);
            }
            if let Some(background_request) = state.background_requests.pop_front() {
                return MainAsrJob::Background(background_request);
            }
            if let Some(stream_request) = state.latest_stream.take() {
                return MainAsrJob::Stream(stream_request);
            }
            Self::sweep_cancelled_files(&mut state);
            if !state.dictation_active {
                if let Some(work) = state.file_works.pop_front() {
                    return MainAsrJob::File(work);
                }
            }
            if keep_warm && state.dictation_active && state.keep_warm_burst > 0 {
                state.keep_warm_burst -= 1;
                return MainAsrJob::KeepWarm;
            }
            if keep_warm && state.dictation_active && state.keep_warm_continuous {
                return MainAsrJob::KeepWarm;
            }
            if keep_warm && state.dictation_active {
                let idle = idle_since.elapsed();
                if idle >= keep_warm_idle {
                    return MainAsrJob::KeepWarm;
                }
                self.ready.wait_for(
                    &mut state,
                    (keep_warm_idle - idle).min(Duration::from_millis(250)),
                );
                continue;
            }
            // ENG-16: with no dictation, file job, lease wait or yieldable lease, every state
            // change that can give this thread work arrives with a notify, so the 250 ms poll
            // only woke an idle engine 4x/s. It stays wherever something is polled.
            if untimed_idle
                && !state.dictation_active
                && state.file_works.is_empty()
                && !crate::speech_service::lease::lease_wait_active()
                && !crate::speech_service::lease::may_yield()
            {
                self.ready.wait(&mut state);
                continue;
            }
            self.ready.wait_for(&mut state, Duration::from_millis(250));
        }
    }
}

#[derive(Default)]
pub(crate) struct ProbeQueueState {
    pub(crate) controls: std::collections::VecDeque<ProbeControl>,
    pub(crate) latest_probe: Option<ProbeRequest>,
    pub(crate) idle_wake: std::collections::VecDeque<IdleWakeRequest>,
}

pub(crate) struct ProbeMailbox {
    pub(crate) state: Mutex<ProbeQueueState>,
    pub(crate) ready: parking_lot::Condvar,
    pub(crate) exclusivity: Arc<ProbeLaneExclusivity>,
}

impl Default for ProbeMailbox {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            ready: parking_lot::Condvar::new(),
            exclusivity: Arc::new(ProbeLaneExclusivity::default()),
        }
    }
}

impl ProbeMailbox {
    pub(crate) fn submit_latest(&self, request: ProbeRequest) -> bool {
        let mut state = self.state.lock();
        if !self.exclusivity.queue_probe() {
            return false;
        }
        #[cfg(test)]
        replay_mark_lane_in_flight(worker_clock::ExecutorLane::Kws, request.submitted_at);
        trace_probe_lifecycle(&request, "queued", None, None);
        if let Some(replaced) = state.latest_probe.replace(request) {
            trace_probe_lifecycle(&replaced, "pending_replaced", None, None);
            #[cfg(test)]
            replay_note_probe_discarded(replaced.total_samples);
        }
        drop(state);
        self.ready.notify_one();
        true
    }

    pub(crate) fn clear_pending(&self) {
        let mut state = self.state.lock();
        if let Some(cleared) = state.latest_probe.take() {
            self.exclusivity.clear_probe_queue();
            trace_probe_lifecycle(&cleared, "pending_cleared", None, None);
            #[cfg(test)]
            replay_note_probe_discarded(cleared.total_samples);
        }
        state.idle_wake.clear();
    }

    pub(crate) fn submit_idle_wake(&self, mut request: IdleWakeRequest) {
        let mut state = self.state.lock();
        if let Some(previous) = state.idle_wake.back_mut() {
            if previous.total_samples == request.start_sample {
                previous.audio.append(&mut request.audio);
                previous.total_samples = request.total_samples;
                drop(state);
                self.ready.notify_one();
                return;
            }
        }
        state.idle_wake.push_back(request);
        drop(state);
        self.ready.notify_one();
    }

    pub(crate) fn push_control(&self, control: ProbeControl) {
        self.state.lock().controls.push_back(control);
        self.ready.notify_one();
    }

    pub(crate) fn recv(&self) -> ProbeJob {
        let mut state = self.state.lock();
        loop {
            if let Some(control) = state.controls.pop_front() {
                return ProbeJob::Control(control);
            }
            if let Some(request) = state.latest_probe.take() {
                self.exclusivity.probe_started();
                trace_probe_lifecycle(&request, "inference_started", None, None);
                return ProbeJob::Probe(request);
            }
            if let Some(request) = state.idle_wake.pop_front() {
                return ProbeJob::IdleWake(request);
            }
            self.ready.wait(&mut state);
        }
    }
}

/// Cross-mailbox ownership for opening ASR & recording KWS. A result remains
/// owned until capture worker drains it, preventing the other lane from
/// starting between decoder completion & result dispatch.
#[derive(Default)]
pub(crate) struct ProbeLaneExclusivity {
    state: Mutex<ProbeLaneExclusivityState>,
}

#[derive(Default)]
struct ProbeLaneExclusivityState {
    opening_queued: bool,
    opening_active: bool,
    opening_result_pending: usize,
    probe_queued: bool,
    probe_active: bool,
    probe_result_pending: usize,
}

impl ProbeLaneExclusivity {
    fn queue_opening(&self) -> bool {
        let mut state = self.state.lock();
        if state.probe_queued || state.probe_active || state.probe_result_pending > 0 {
            return false;
        }
        state.opening_queued = true;
        true
    }

    fn queue_probe(&self) -> bool {
        let mut state = self.state.lock();
        if state.opening_queued || state.opening_active || state.opening_result_pending > 0 {
            return false;
        }
        state.probe_queued = true;
        true
    }

    fn clear_opening_queue(&self) {
        self.state.lock().opening_queued = false;
    }

    fn clear_probe_queue(&self) {
        self.state.lock().probe_queued = false;
    }

    fn opening_started(&self) {
        let mut state = self.state.lock();
        state.opening_queued = false;
        state.opening_active = true;
    }

    fn probe_started(&self) {
        let mut state = self.state.lock();
        state.probe_queued = false;
        state.probe_active = true;
    }

    pub(crate) fn opening_finished(&self) {
        let mut state = self.state.lock();
        state.opening_active = false;
        state.opening_result_pending = state.opening_result_pending.saturating_add(1);
    }

    pub(crate) fn opening_result_drained(&self) {
        let mut state = self.state.lock();
        assert!(
            state.opening_result_pending > 0,
            "opening result drained without pending result"
        );
        state.opening_result_pending -= 1;
    }

    pub(crate) fn probe_finished(&self) {
        let mut state = self.state.lock();
        state.probe_active = false;
        state.probe_result_pending = state.probe_result_pending.saturating_add(1);
    }

    pub(crate) fn probe_result_drained(&self) {
        let mut state = self.state.lock();
        assert!(
            state.probe_result_pending > 0,
            "KWS result drained without pending result"
        );
        state.probe_result_pending -= 1;
    }

    pub(crate) fn opening_clear(&self) -> bool {
        let state = self.state.lock();
        !state.opening_queued && !state.opening_active && state.opening_result_pending == 0
    }
}

pub(crate) struct AsrExecutor {
    pub(crate) main_mailbox: Arc<MainAsrMailbox>,
    pub(crate) probe_mailbox: Arc<ProbeMailbox>,
    pub(crate) result_rx: Receiver<ProbeResult>,
    pub(crate) idle_wake_result_rx: Receiver<IdleWakeResult>,
    pub(crate) opening_result_rx: Receiver<OpeningCommandResult>,
    pub(crate) stream_result_rx: Receiver<StreamResult>,
    pub(crate) main_alive: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) probe_alive: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) scheduled_static15_enabled: bool,
    pub(crate) control_cleanup_supported: Arc<std::sync::atomic::AtomicBool>,
    reload_tx: Sender<ReloadCoordinatorCommand>,
    reload_result_rx: Receiver<ReloadCompletion>,
    reload_next_id: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    reload_lock: Arc<std::sync::Mutex<()>>,
}

pub(crate) struct AsrWorkerAlive {
    alive: Arc<std::sync::atomic::AtomicBool>,
    wait_signal: Arc<WorkerWaitSignal>,
}

impl Drop for AsrWorkerAlive {
    fn drop(&mut self) {
        self.alive
            .store(false, std::sync::atomic::Ordering::Release);
        self.wait_signal.notify();
    }
}

impl AsrExecutor {
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn spawn(models_dir: PathBuf, ep: AsrEp) -> Result<Self, String> {
        Self::spawn_with_wait_signal(models_dir, ep, Arc::new(WorkerWaitSignal::default()))
    }

    pub(crate) fn spawn_with_wait_signal(
        models_dir: PathBuf,
        ep: AsrEp,
        wait_signal: Arc<WorkerWaitSignal>,
    ) -> Result<Self, String> {
        let exclusivity = Arc::new(ProbeLaneExclusivity::default());
        let main_mailbox = Arc::new(MainAsrMailbox {
            state: Mutex::default(),
            ready: parking_lot::Condvar::new(),
            exclusivity: Arc::clone(&exclusivity),
        });
        let main_worker_mailbox = Arc::clone(&main_mailbox);
        let (main_ready_tx, main_ready_rx) = channel::<Result<(bool, bool), String>>();
        let (opening_result_tx, opening_result_rx) = channel::<OpeningCommandResult>();
        let (stream_result_tx, stream_result_rx) = channel::<StreamResult>();
        let main_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let main_worker_alive = Arc::clone(&main_alive);
        let main_wait_signal = Arc::clone(&wait_signal);
        let main_models_dir = models_dir.clone();
        #[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
        let trigger_replay = TRIGGER_REPLAY_ACTIVE.load(std::sync::atomic::Ordering::Acquire);

        std::thread::Builder::new()
            .name("hr-main-asr-worker".to_string())
            .spawn(move || {
                let _alive_guard = AsrWorkerAlive {
                    alive: main_worker_alive,
                    wait_signal: Arc::clone(&main_wait_signal),
                };
                crate::perf_levers::apply_thread_qos("hr-main-asr-worker");
                #[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
                let mut model = if trigger_replay {
                    AsrRuntime::ProbeDisabled
                } else {
                    match AsrRuntime::load(&main_models_dir, ep) {
                        Ok(model) => model,
                        Err(error) => {
                            let _ = main_ready_tx
                                .send(Err(format!("main ASR model load: {error}")));
                            return;
                        }
                    }
                };
                #[cfg(not(all(test, any(target_os = "macos", target_os = "windows"))))]
                let mut model = match AsrRuntime::load(&main_models_dir, ep) {
                    Ok(model) => model,
                    Err(error) => {
                        let _ =
                            main_ready_tx.send(Err(format!("main ASR model load: {error}")));
                        return;
                    }
                };
                if let Err(error) = warm_main_asr(&mut model) {
                    let _ = main_ready_tx.send(Err(format!("main ASR warmup: {error}")));
                    return;
                }
                let scheduled_enabled = model.uses_scheduled_static15()
                    && rightkit_dictation_core::dev_var!("RIGHTKIT_SCHEDULED_STATIC15").ok().as_deref() != Some("0");
                let mut control_cleanup_supported = model.supports_control_cleanup();
                let mut last_model_key = asr_reload_key();
                let mut scheduled_static = ScheduledStatic15::default();
                let mut active_generation: Option<u64> = None;
                // Retain a failed window until it commits. Later windows stay
                // unprocessed rather than creating a gap in committed audio.
                let mut deferred_background: Option<BackgroundRequest> = None;
                // Whisper AUTO language, locked from whatever the first decode
                // of this recording resolves. Reset alongside `scheduled_static`
                // so a new recording can detect a different language again.
                let mut whisper_locked_lang: Option<i32> = None;
                let _ = main_ready_tx.send(Ok((scheduled_enabled, control_cleanup_supported)));

                let mut lease_unloaded = false;
                loop {
                    let job = main_worker_mailbox.recv();
                    if matches!(job, MainAsrJob::LeaseYield) {
                        model = AsrRuntime::ProbeDisabled;
                        crate::speech_service::lease::release();
                        lease_unloaded = true;
                        continue;
                    }
                    if lease_unloaded && !matches!(job, MainAsrJob::Control(MainAsrControl::Shutdown | MainAsrControl::Cancel { .. })) {
                        model = match AsrRuntime::load(&main_models_dir, ep).and_then(|mut m| { warm_main_asr(&mut m)?; Ok(m) }) {
                            Ok(model) => model,
                            Err(error) => {
                                tracing::error!(event = "service_model_resume_failed", %error);
                                main_worker_mailbox.fail_all_file_jobs(&error);
                                crate::speech_service::lease::release();
                                break;
                            }
                        };
                        lease_unloaded = false;
                        tracing::info!(event = "service_model_resumed");
                    }
                    match job {
                        MainAsrJob::LeaseYield => unreachable!(),
                        MainAsrJob::Final(FinalRequest::Dictation {
                            generation,
                            audio,
                            reply,
                        }) => {
                            rightkit_dictation_inference::timeline_recorder::mark(|t| &mut t.final_started_ms);
                            if active_generation != Some(generation) {
                                scheduled_static.reset();
                                whisper_locked_lang = None;
                            }
                            let result = finish_recording_transcript(
                                &mut model,
                                &mut scheduled_static,
                                &audio,
                                generation,
                                &mut whisper_locked_lang,
                            );
                            scheduled_static.reset();
                            active_generation = None;
                            deferred_background = None;
                            whisper_locked_lang = None;
                            let _ = reply.send(result);
                        }
                        MainAsrJob::Final(FinalRequest::ZephyrControl {
                            generation,
                            audio,
                            onset_sample,
                            intent,
                            reply,
                        }) => {
                            if active_generation != Some(generation) {
                                scheduled_static.reset();
                                whisper_locked_lang = None;
                            }
                            let result = finish_recording_transcript(
                                &mut model,
                                &mut scheduled_static,
                                &audio,
                                generation,
                                &mut whisper_locked_lang,
                            )
                            .map(|full| {
                                if !control_cleanup_supported {
                                    let original_text = full.text.clone();
                                    let stripped = rightkit_text_polish::text_pipeline::strip_fired_control_tail(
                                        &original_text,
                                        intent,
                                    );
                                    let mut transcript = full;
                                    if stripped != transcript.text {
                                        transcript.tokens.clear();
                                        transcript.text = stripped;
                                    }
                                    tracing::warn!(
                                        generation,
                                        "confirmed control cleanup backend unavailable; using text-only fallback"
                                    );
                                    return FinalControlResult {
                                        original_text,
                                        transcript,
                                        method: "unsupported_backend",
                                        boundary_sec: None,
                                        score: None,
                                        joint_calls: 0,
                                        error: None,
                                    };
                                }
                                let original_text = full.text.clone();
                                let cleanup = rightkit_dictation_inference::control_cleanup::cleanup_confirmed_control_with_exact_retry(
                                    full,
                                    &audio,
                                    intent,
                                    onset_sample,
                                    |conditioned, command, earliest_sec, exact_only| {
                                        model.with_inference_lease("control_alignment", |model| {
                                            crate::asr::apply_utterance_bias(model);
                                            #[cfg(target_os = "windows")]
                                            if exact_only {
                                                return model.align_control_suffix_exact_under_lease(
                                                    conditioned, command, earliest_sec,
                                                );
                                            }
                                            let _ = exact_only;
                                            model.align_control_suffix_under_lease(conditioned, command, earliest_sec)
                                        })
                                    },
                                );
                                FinalControlResult {
                                    original_text,
                                    transcript: cleanup.transcript,
                                    method: cleanup.method,
                                    boundary_sec: cleanup.boundary_sec,
                                    score: cleanup.score,
                                    joint_calls: cleanup.joint_calls,
                                    error: cleanup.error,
                                }
                            });
                            scheduled_static.reset();
                            active_generation = None;
                            deferred_background = None;
                            whisper_locked_lang = None;
                            let _ = reply.send(result);
                        }
                        MainAsrJob::File(mut work) => {
                            use std::sync::atomic::Ordering::{Acquire, Release};
                            if let Some(code) = main_worker_mailbox.file_failure_for(&work) {
                                let _ = work.reply.send(Err(code));
                                continue;
                            }
                            if work.cancel.load(Acquire) {
                                let _ = work.reply.send(Err(FILE_CANCELLED.to_string()));
                                continue;
                            }
                            work.progress.started.store(true, Release);
                            let step = model
                                .with_inference_lease("file_asr", |model| {
                                    crate::asr::apply_utterance_bias(model);
                                    Ok(model.file_step(&mut work.job))
                                })
                                .unwrap_or_else(|error| crate::asr::FileStep::Done(Err(error)));
                            work.progress.done.store(work.job.done_samples(), Release);
                            match step {
                                crate::asr::FileStep::Pending => {
                                    main_worker_mailbox.requeue_file(work);
                                }
                                crate::asr::FileStep::Done(result) => {
                                    // Blank output keeps today's contract: the
                                    // audible-blank error, not an empty transcript.
                                    let result = result.and_then(|transcript| {
                                        if transcript.text.trim().is_empty() {
                                            Err(crate::asr::AUDIBLE_BLANK_TRANSCRIPTION_ERROR
                                                .to_string())
                                        } else {
                                            Ok(transcript)
                                        }
                                    });
                                    let _ = work.reply.send(result);
                                }
                            }
                        }
                        MainAsrJob::Control(MainAsrControl::Reload { reply }) => {
                            if asr_reload_key() != last_model_key {
                                main_worker_mailbox.fail_all_file_jobs(FILE_RELOADED);
                            }
                            let result = reload_asr_if_changed(
                                &mut model,
                                &mut last_model_key,
                                &main_models_dir,
                                ep,
                            );
                            let support = model.supports_control_cleanup();
                            control_cleanup_supported = support;
                            let _ = reply.send(
                                result.map(|changed| (changed, support, last_model_key.clone())),
                            );
                        }
                        MainAsrJob::Control(MainAsrControl::Cancel { generation }) => {
                            if active_generation == Some(generation) {
                                scheduled_static.reset();
                                active_generation = None;
                                deferred_background = None;
                                whisper_locked_lang = None;
                            }
                        }
                        MainAsrJob::Control(MainAsrControl::Shutdown) => break,
                        MainAsrJob::KeepWarm => {
                            let started = Instant::now();
                            let result = keep_warm_main_asr(&mut model);
                            tracing::debug!(
                                elapsed_ms = started.elapsed().as_millis() as u64,
                                ok = result.is_ok(),
                                "asr_keep_warm"
                            );
                        }
                        MainAsrJob::Opening(request) => {
                            let queue_ms = request.created_at.elapsed().as_millis() as u64;
                            let started = Instant::now();
                            let transcript = transcribe_opening_buffer(&mut model, &request.audio);
                            let measured_decode_duration = started.elapsed();
                            tracing::info!(
                                generation = request.generation,
                                queue_ms,
                                decode_ms = measured_decode_duration.as_millis() as u64,
                                "opening command main ASR decode complete"
                            );
                            let opening_result = OpeningCommandResult {
                                request,
                                transcript,
                                decode_ms: measured_decode_duration.as_millis() as u64,
                                #[cfg(test)]
                                measured_decode_duration,
                            };
                            #[cfg(test)]
                            {
                                let completion = opening_result.lane_completion();
                                replay_schedule_lane_completion(completion);
                                while !replay_lane_completion_ready(completion) {
                                    std::thread::yield_now();
                                }
                            }
                            main_worker_mailbox.exclusivity.opening_finished();
                            let _ = opening_result_tx.send(opening_result);
                            main_wait_signal.notify();
                        }
                        MainAsrJob::Stream(request) => {
                            let started = Instant::now();
                            let end_sample = request.start_sample + request.audio.len();
                            let result = transcribe_buffer_result(&mut model, &request.audio, &mut None);
                            tracing::debug!(
                                generation = request.generation,
                                window_samples = request.audio.len(),
                                decode_ms = started.elapsed().as_millis() as u64,
                                ok = result.is_ok(),
                                "streaming insertion decode"
                            );
                            let _ = stream_result_tx.send(StreamResult {
                                generation: request.generation,
                                start_sample: request.start_sample,
                                end_sample,
                                result,
                            });
                            main_wait_signal.notify();
                        }
                        MainAsrJob::Background(request) => {
                            if !scheduled_enabled {
                                continue;
                            }
                            if active_generation != Some(request.generation) {
                                scheduled_static.reset();
                                active_generation = Some(request.generation);
                                deferred_background = None;
                                whisper_locked_lang = None;
                            }
                            if let Some(retry) = deferred_background.take() {
                                let started = Instant::now();
                                match commit_background_window(
                                    &mut scheduled_static,
                                    &retry,
                                    |audio| {
                                        transcribe_buffer_result(
                                            &mut model,
                                            audio,
                                            &mut whisper_locked_lang,
                                        )
                                    },
                                ) {
                                    Ok(text) => tracing::info!(
                                        generation = retry.generation,
                                        start_sample = retry.start_sample,
                                        end_sample = retry.end_sample,
                                        window_samples = retry.audio.len(),
                                        window_chars = text.chars().count(),
                                        window_words = text.split_whitespace().count(),
                                        decode_ms = started.elapsed().as_millis() as u64,
                                        "scheduled static-15 background window recovered"
                                    ),
                                    Err(error) => {
                                        tracing::error!(
                                            generation = retry.generation,
                                            start_sample = retry.start_sample,
                                            end_sample = retry.end_sample,
                                            window_samples = retry.audio.len(),
                                            error,
                                            "scheduled static-15 background retry failed; retaining contiguous tail for final decode"
                                        );
                                        deferred_background = Some(retry);
                                        continue;
                                    }
                                }
                            }
                            let started = Instant::now();
                            match commit_background_window(
                                &mut scheduled_static,
                                &request,
                                |audio| {
                                    transcribe_buffer_result(
                                        &mut model,
                                        audio,
                                        &mut whisper_locked_lang,
                                    )
                                },
                            ) {
                                Ok(text) => {
                                        let (committed_chars, committed_words) =
                                            scheduled_static.committed_text_counts();
                                        tracing::info!(
                                            generation = request.generation,
                                            start_sample = request.start_sample,
                                            end_sample = request.end_sample,
                                            window_samples = request.audio.len(),
                                            window_chars = text.chars().count(),
                                            window_words = text.split_whitespace().count(),
                                            window_empty = text.trim().is_empty(),
                                            background_windows =
                                                scheduled_static.background_windows(),
                                            committed_samples =
                                                scheduled_static.committed_samples(),
                                            committed_chars,
                                            committed_words,
                                            decode_ms = started.elapsed().as_millis() as u64,
                                            "scheduled static-15 background decode complete"
                                        );
                                }
                                Err(error) => {
                                    tracing::error!(
                                        generation = request.generation,
                                        start_sample = request.start_sample,
                                        end_sample = request.end_sample,
                                        window_samples = request.audio.len(),
                                        error,
                                        "scheduled static-15 background window failed; retaining it for retry before later work"
                                    );
                                    deferred_background = Some(request);
                                }
                            }
                        }
                    }
                }
            })
            .map_err(|error| format!("spawn main ASR worker: {error}"))?;

        // ORT-04 / ENG-13 (`RIGHTKIT_PARALLEL_MODEL_LOAD=1`): spawn the KWS worker while the main ASR
        // is still loading and join both below. Default: wait here first, as shipped. ORT is
        // initialised in `entry` before any worker exists, so the order carries no init hazard.
        let parallel_load = crate::perf_levers::parallel_model_load();
        let recv_main_ready = |rx: &std::sync::mpsc::Receiver<Result<(bool, bool), String>>| {
            rx.recv()
                .map_err(|_| "main ASR worker exited before ready".to_string())?
        };
        let main_ready_serial = if parallel_load {
            None
        } else {
            Some(recv_main_ready(&main_ready_rx)?)
        };

        let probe_mailbox = Arc::new(ProbeMailbox {
            state: Mutex::default(),
            ready: parking_lot::Condvar::new(),
            exclusivity,
        });
        let probe_worker_mailbox = Arc::clone(&probe_mailbox);
        let (result_tx, result_rx) = channel::<ProbeResult>();
        let (idle_wake_result_tx, idle_wake_result_rx) = channel::<IdleWakeResult>();
        let (probe_ready_tx, probe_ready_rx) = channel::<Result<(), String>>();
        let probe_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let probe_worker_alive = Arc::clone(&probe_alive);
        let probe_wait_signal = Arc::clone(&wait_signal);

        std::thread::Builder::new()
            .name("hr-kws-worker".to_string())
            .spawn(move || {
                let _alive_guard = AsrWorkerAlive {
                    alive: probe_worker_alive,
                    wait_signal: Arc::clone(&probe_wait_signal),
                };
                crate::perf_levers::apply_thread_qos("hr-kws-worker");
                let mut probe_model = match AsrRuntime::load_probe(&models_dir, ep) {
                    Ok(probe_model) => probe_model,
                    Err(error) => {
                        let _ = probe_ready_tx.send(Err(format!("KWS model load: {error}")));
                        return;
                    }
                };
                if let Err(error) = transcribe_probe_buffer_timed(
                    &mut probe_model,
                    &vec![0.0; SAMPLE_RATE as usize],
                ) {
                    let _ = probe_ready_tx.send(Err(format!("KWS warmup: {error}")));
                    return;
                }
                if let Err(error) = probe_model.reset_probe_stream() {
                    let _ = probe_ready_tx.send(Err(format!("KWS stream reset: {error}")));
                    return;
                }
                crate::asr::apply_probe_context_bias(&mut probe_model);
                let mut probe_last_model_key = asr_reload_key();
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                let mut probe_session_id = String::new();
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                let mut probe_processed_total = 0usize;
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                let mut probe_stream_base_sample = 0usize;
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                let mut probe_stream_generation = 0u64;
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                let mut probe_native_origin_sample = 0usize;
                let _ = probe_ready_tx.send(Ok(()));

                loop {
                    match probe_worker_mailbox.recv() {
                        ProbeJob::Control(ProbeControl::Reload { reply }) => {
                            let result = reload_probe_asr_if_changed(
                                &mut probe_model,
                                &mut probe_last_model_key,
                                &models_dir,
                                ep,
                            );
                            if matches!(result, Ok(true)) {
                                crate::asr::apply_probe_context_bias(&mut probe_model);
                                let _ = probe_model.reset_probe_stream();
                                #[cfg(any(target_os = "macos", target_os = "windows"))]
                                {
                                    probe_session_id.clear();
                                    probe_processed_total = 0;
                                    probe_stream_base_sample = 0;
                                    probe_stream_generation = 0;
                                    probe_native_origin_sample = 0;
                                }
                            }
                            let _ = reply.send(
                                result.map(|changed| (changed, probe_last_model_key.clone())),
                            );
                        }
                        ProbeJob::Control(ProbeControl::Reset) => {
                            let _ = probe_model.reset_probe_stream();
                            #[cfg(any(target_os = "macos", target_os = "windows"))]
                            {
                                probe_session_id.clear();
                                probe_processed_total = 0;
                                probe_stream_base_sample = 0;
                                probe_stream_generation = 0;
                                probe_native_origin_sample = 0;
                            }
                        }
                        ProbeJob::Control(ProbeControl::StartIdleWake { threshold, reply }) => {
                            let _ = reply.send(probe_model.start_idle_wake(threshold));
                        }
                        ProbeJob::Control(ProbeControl::StopIdleWake { reply }) => {
                            probe_model.stop_idle_wake();
                            let _ = reply.send(());
                        }
                        ProbeJob::Control(ProbeControl::Shutdown) => break,
                        ProbeJob::IdleWake(request) => {
                            let started = Instant::now();
                            let hit = probe_model.feed_idle_wake(
                                &request.audio,
                                request.start_sample,
                                request.total_samples,
                            );
                            let _ = idle_wake_result_tx.send(IdleWakeResult {
                                hit,
                                total_samples: request.total_samples,
                                decode_ms: started.elapsed().as_millis() as u64,
                            });
                            probe_wait_signal.notify();
                        }
                        ProbeJob::Probe(request) => {
                            #[cfg(test)]
                            replay_mark_lane_started(
                                worker_clock::ExecutorLane::Kws,
                                request.submitted_at,
                            );
                            let started = Instant::now();
                            let queue_wait_ms =
                                started.duration_since(request.created_at).as_millis() as u64;
                            // Prefer the timestamped decode so the trigger word's onset
                            // is known without a second pass over the same audio (see
                            // `find_trigger_onset_sample`). Backends without timed
                            // support (Whisper CoreML/Win) return `Err` here immediately
                            // — no real decode is attempted — so falling back to the
                            // plain-text probe costs nothing extra on that path and
                            // command/trigger classification is completely unaffected.
                            let probe_model = &mut probe_model;
                            #[cfg(any(target_os = "macos", target_os = "windows"))]
                            let kws_cascade = probe_model.requires_timed_control_probe();
                            #[cfg(any(target_os = "macos", target_os = "windows"))]
                            let probe_offset = incremental_probe_offset(
                                probe_processed_total,
                                request.command_start,
                                request.audio.len(),
                            );
                            #[cfg(any(target_os = "macos", target_os = "windows"))]
                            let probe_reset = request.session_id != probe_session_id
                                || request.kws_stream_generation != probe_stream_generation
                                || (!kws_cascade
                                    && (request.stream_base_sample != probe_stream_base_sample
                                        || request.native_origin_sample
                                            != probe_native_origin_sample
                                        || probe_offset.is_none()));
                            #[cfg(any(target_os = "macos", target_os = "windows"))]
                            let reset_stream_base_sample = probe_reset_stream_base(
                                request.stream_base_sample,
                                request.command_start,
                                probe_offset,
                            );
                            #[cfg(any(target_os = "macos", target_os = "windows"))]
                            let prepare_error = if probe_reset {
                                match probe_model.reset_probe_stream() {
                                    Ok(()) => {
                                        probe_session_id.clone_from(&request.session_id);
                                        probe_stream_base_sample = reset_stream_base_sample;
                                        probe_stream_generation = request.kws_stream_generation;
                                        // Native reset feeds probe_audio from index zero,
                                        // whose absolute cursor is command_start.
                                        probe_native_origin_sample = request.command_start;
                                        None
                                    }
                                    Err(error) => Some(error),
                                }
                            } else {
                                None
                            };
                            #[cfg(any(target_os = "macos", target_os = "windows"))]
                            let probe_audio_offset = if kws_cascade || probe_reset {
                                0
                            } else {
                                probe_offset.unwrap_or(0)
                            };
                            #[cfg(any(target_os = "macos", target_os = "windows"))]
                            let probe_audio = &request.audio[probe_audio_offset..];
                            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                            let probe_audio = request.audio.as_slice();
                            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                            let prepare_error: Option<String> = None;
                            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                            let probe_stream_base_sample = request.command_start;
                            let (
                                transcript,
                                trigger_onset_sample,
                                trigger_onset_skip_reason,
                                native_control_intent,
                            ) = if let Some(error) = prepare_error {
                                (Err(error), None, Some("probe_stream_reset_failed"), None)
                            } else {
                                let timed_result = if kws_cascade {
                                    probe_native_origin_sample = request.command_start;
                                    probe_model
                                        .transcribe_scheduled_probe_result_with_wake_start(
                                            request.audio.as_slice(),
                                            request.command_start,
                                            request.total_samples,
                                            request.pending_control_prefix,
                                            should_use_fresh_confirmation_fallback(
                                                request.lane,
                                                request.pending_control_prefix,
                                            ),
                                        )
                                        .map(|(result, wake_start, _fresh_used)| {
                                            (result, wake_start)
                                        })
                                } else {
                                    transcribe_probe_buffer_timed(probe_model, probe_audio)
                                        .map(|result| (result, None))
                                };
                                match timed_result {
                                    Ok((timed, explicit_wake_start)) => {
                                        let onset = resolve_trigger_onset_sample(
                                            explicit_wake_start,
                                            probe_native_origin_sample,
                                            &timed.tokens,
                                        );
                                        let skip_reason = if onset.is_some() {
                                            None
                                        } else {
                                            Some("piece_not_found")
                                        };
                                        let native_intent = native_control_intent(&timed.text);
                                        (Ok(timed.text), onset, skip_reason, native_intent)
                                    }
                                    Err(timed_err)
                                        if probe_model.requires_timed_control_probe() =>
                                    {
                                        // Never fire a KWS control without its emission-frame
                                        // timing. A text-only retry would make final TDT decode
                                        // command audio that must have been cut first.
                                        (Err(timed_err), None, Some("timed_probe_failed"), None)
                                    }
                                    Err(_timed_err) => {
                                        let text =
                                            transcribe_probe_buffer(probe_model, probe_audio);
                                        (text, None, Some("no_timestamps"), None)
                                    }
                                }
                            };
                            #[cfg(any(target_os = "macos", target_os = "windows"))]
                            if transcript.is_ok() {
                                probe_processed_total = request.total_samples;
                            } else {
                                // A failed timed KWS result can mean KWS found
                                // an implausibly long-lived keyword path. Drop all
                                // stream identity so the next rolling request
                                // rebuilds from its own snapshot start; retaining
                                // the old base would make new timestamps point at
                                // unrelated earlier audio.
                                let _ = probe_model.reset_probe_stream();
                                probe_session_id.clear();
                                probe_processed_total = 0;
                                probe_stream_base_sample = 0;
                                probe_stream_generation = 0;
                                probe_native_origin_sample = 0;
                            }
                            let probe_ms = started.elapsed().as_millis() as u64;
                            #[cfg(test)]
                            let measured_decode_duration = started.elapsed();
                            trace_probe_lifecycle(
                                &request,
                                "inference_finished",
                                Some(probe_ms),
                                Some(transcript.is_ok()),
                            );
                            let mut probe_result = ProbeResult {
                                request,
                                transcript,
                                probe_ms,
                                queue_wait_ms,
                                completed_at: Instant::now(),
                                #[cfg(test)]
                                measured_decode_duration,
                                #[cfg(any(target_os = "macos", target_os = "windows"))]
                                native_origin_sample: probe_native_origin_sample,
                                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                                native_origin_sample: request.command_start,
                                native_control_intent,
                                trigger_onset_sample,
                                trigger_onset_skip_reason,
                            };
                            #[cfg(test)]
                            {
                                let completion = probe_result.lane_completion();
                                replay_schedule_lane_completion(completion);
                                while !replay_lane_completion_ready(completion) {
                                    std::thread::yield_now();
                                }
                            }
                            probe_result.completed_at = Instant::now();
                            probe_worker_mailbox.exclusivity.probe_finished();
                            let _ = result_tx.send(probe_result);
                            probe_wait_signal.notify();
                        }
                    }
                }
            })
            .map_err(|error| {
                if parallel_load {
                    main_mailbox.push_control(MainAsrControl::Shutdown);
                }
                format!("spawn KWS worker: {error}")
            })?;

        let (scheduled_static15_enabled, control_cleanup_supported) = match main_ready_serial {
            Some(ready) => ready,
            None => {
                let started = Instant::now();
                match recv_main_ready(&main_ready_rx) {
                    Ok(ready) => {
                        tracing::info!(
                            event = "parallel_model_load",
                            main_wait_ms = started.elapsed().as_millis() as u64,
                            "main ASR ready (KWS loading in parallel)"
                        );
                        ready
                    }
                    Err(error) => {
                        probe_mailbox.push_control(ProbeControl::Shutdown);
                        return Err(error);
                    }
                }
            }
        };

        if let Err(error) = probe_ready_rx
            .recv()
            .map_err(|_| "KWS worker exited before ready".to_string())?
        {
            main_mailbox.push_control(MainAsrControl::Shutdown);
            return Err(error);
        }
        let control_cleanup_supported = Arc::new(std::sync::atomic::AtomicBool::new(
            control_cleanup_supported,
        ));
        let (reload_tx, reload_rx) = channel::<ReloadCoordinatorCommand>();
        let (reload_result_tx, reload_result_rx) = channel::<ReloadCompletion>();
        let reload_main_mailbox = Arc::clone(&main_mailbox);
        let reload_probe_mailbox = Arc::clone(&probe_mailbox);
        let reload_main_alive = Arc::clone(&main_alive);
        let reload_probe_alive = Arc::clone(&probe_alive);
        let reload_cleanup_supported = Arc::clone(&control_cleanup_supported);
        let reload_thread_lock = Arc::new(std::sync::Mutex::new(()));
        #[cfg(test)]
        let reload_lock = Arc::clone(&reload_thread_lock);
        let reload_wait_signal = Arc::clone(&wait_signal);
        std::thread::Builder::new()
            .name("hr-asr-reload-coordinator".to_string())
            .spawn(move || {
                tracing::info!(phase = "started", "ASR reload coordinator");
                run_reload_coordinator(
                    reload_rx,
                    reload_result_tx,
                    || {
                        let deadline = Instant::now() + Duration::from_secs(61);
                        let _guard = loop {
                            match reload_thread_lock.try_lock() {
                                Ok(guard) => break guard,
                                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                                    break poisoned.into_inner()
                                }
                                Err(std::sync::TryLockError::WouldBlock) => {
                                    if Instant::now() >= deadline {
                                        return Err(
                                            "ASR reload coordinator lock deadline exceeded"
                                                .to_string(),
                                        );
                                    }
                                    std::thread::sleep(Duration::from_millis(2));
                                }
                            }
                        };
                        reload_model_until_stable(
                            &reload_main_mailbox,
                            &reload_probe_mailbox,
                            &reload_main_alive,
                            &reload_probe_alive,
                            &reload_cleanup_supported,
                            deadline,
                        )
                    },
                    Some(reload_wait_signal),
                );
            })
            .map_err(|error| {
                tracing::error!(error = %error, phase = "spawn_failed", "ASR reload coordinator");
                probe_mailbox.push_control(ProbeControl::Shutdown);
                main_mailbox.push_control(MainAsrControl::Shutdown);
                format!("spawn ASR reload coordinator: {error}")
            })?;
        Ok(Self {
            main_mailbox,
            probe_mailbox,
            result_rx,
            idle_wake_result_rx,
            opening_result_rx,
            stream_result_rx,
            main_alive,
            probe_alive,
            scheduled_static15_enabled,
            control_cleanup_supported,
            reload_tx,
            reload_result_rx,
            reload_next_id: std::sync::atomic::AtomicU64::new(1),
            #[cfg(test)]
            reload_lock,
        })
    }

    pub(crate) fn submit_probe(&self, request: ProbeRequest) -> bool {
        self.probe_mailbox.submit_latest(request)
    }

    pub(crate) fn opening_lane_clear(&self) -> bool {
        self.main_mailbox.exclusivity.opening_clear()
    }

    pub(crate) fn opening_result_drained(&self) {
        self.main_mailbox.exclusivity.opening_result_drained();
    }

    pub(crate) fn probe_result_drained(&self) {
        self.probe_mailbox.exclusivity.probe_result_drained();
    }

    pub(crate) fn start_idle_wake(&self, threshold: f32) -> Result<(), String> {
        let (reply, result) = channel();
        self.probe_mailbox
            .push_control(ProbeControl::StartIdleWake { threshold, reply });
        result
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| "idle wake start timed out".to_string())?
    }

    pub(crate) fn stop_idle_wake(&self) {
        self.probe_mailbox.clear_pending();
        let (reply, result) = channel();
        self.probe_mailbox
            .push_control(ProbeControl::StopIdleWake { reply });
        let _ = result.recv_timeout(Duration::from_secs(2));
    }

    pub(crate) fn submit_idle_wake(
        &self,
        audio: Vec<f32>,
        start_sample: usize,
        total_samples: usize,
    ) {
        if self.probe_alive.load(std::sync::atomic::Ordering::Acquire) {
            self.probe_mailbox.submit_idle_wake(IdleWakeRequest {
                audio,
                start_sample,
                total_samples,
            });
        }
    }

    pub(crate) fn try_recv_idle_wake(&self) -> Result<Option<IdleWakeResult>, String> {
        match self.idle_wake_result_rx.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(None),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("idle wake result channel disconnected".into())
            }
        }
    }

    pub(crate) fn submit_opening(&self, request: OpeningCommandRequest) -> bool {
        if self.main_alive.load(std::sync::atomic::Ordering::Acquire) {
            self.main_mailbox.submit_opening(request)
        } else {
            false
        }
    }

    pub(crate) fn scheduled_static15_enabled(&self) -> bool {
        self.scheduled_static15_enabled
    }

    pub(crate) fn supports_control_cleanup(&self) -> bool {
        self.control_cleanup_supported
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn submit_background(
        &self,
        generation: u64,
        start_sample: usize,
        end_sample: usize,
        audio: Vec<f32>,
    ) {
        if self.scheduled_static15_enabled
            && self.main_alive.load(std::sync::atomic::Ordering::Acquire)
        {
            self.main_mailbox.submit_background(BackgroundRequest {
                generation,
                start_sample,
                end_sample,
                audio,
            });
        }
    }

    pub(crate) fn submit_stream(&self, generation: u64, start_sample: usize, audio: Vec<f32>) {
        if self.main_alive.load(std::sync::atomic::Ordering::Acquire) {
            self.main_mailbox.submit_stream(StreamRequest {
                generation,
                start_sample,
                audio,
            });
        }
    }

    pub(crate) fn try_recv_stream(&self) -> Option<StreamResult> {
        self.stream_result_rx.try_recv().ok()
    }

    pub(crate) fn clear_pending(&self) {
        self.probe_mailbox.clear_pending();
        let mut state = self.main_mailbox.state.lock();
        state.latest_stream = None;
        if state.latest_opening.take().is_some() {
            self.main_mailbox.exclusivity.clear_opening_queue();
        }
        drop(state);
        // Dropped stale results must release their counted lane ownership.
        while self.opening_result_rx.try_recv().is_ok() {
            self.main_mailbox.exclusivity.opening_result_drained();
        }
        while self.result_rx.try_recv().is_ok() {
            self.probe_mailbox.exclusivity.probe_result_drained();
        }
    }

    pub(crate) fn cancel_recording(&self, generation: u64) {
        self.main_mailbox
            .push_control(MainAsrControl::Cancel { generation });
    }

    pub(crate) fn reset_probe_stream(&self) {
        self.probe_mailbox.clear_pending();
        self.probe_mailbox.push_control(ProbeControl::Reset);
    }

    pub(crate) fn try_recv(&self) -> Result<Option<ProbeResult>, String> {
        match self.result_rx.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err("KWS worker result channel disconnected".to_string())
            }
        }
    }

    pub(crate) fn try_recv_opening(&self) -> Result<Option<OpeningCommandResult>, String> {
        match self.opening_result_rx.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err("main ASR opening result channel disconnected".to_string())
            }
        }
    }

    pub(crate) fn request_reload(&self) -> Result<u64, String> {
        if !self.main_alive.load(std::sync::atomic::Ordering::Acquire)
            || !self.probe_alive.load(std::sync::atomic::Ordering::Acquire)
        {
            return Err("ASR workers are not running".to_string());
        }
        let id = self
            .reload_next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.reload_tx
            .send(ReloadCoordinatorCommand::Request { id })
            .map_err(|_| "ASR reload coordinator is not running".to_string())?;
        Ok(id)
    }

    pub(crate) fn try_recv_reload(&self) -> Result<Option<ReloadCompletion>, String> {
        match self.reload_result_rx.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err("ASR reload coordinator disconnected".to_string())
            }
        }
    }

    pub(crate) fn transcribe_final_result(
        &self,
        generation: u64,
        audio: Vec<f32>,
        cancel_epoch: &std::sync::atomic::AtomicU64,
        handled_cancel_epoch: u64,
    ) -> Result<TranscriptionResult, String> {
        let reply_rx =
            self.queue_final_result(generation, audio, cancel_epoch, handled_cancel_epoch)?;
        self.wait_final_result(generation, reply_rx, cancel_epoch, handled_cancel_epoch)
    }

    /// First half of `transcribe_final_result`: queue the dictation final and return its reply.
    pub(crate) fn queue_final_result(
        &self,
        generation: u64,
        audio: Vec<f32>,
        cancel_epoch: &std::sync::atomic::AtomicU64,
        handled_cancel_epoch: u64,
    ) -> Result<Receiver<Result<TranscriptionResult, String>>, String> {
        if !self.main_alive.load(std::sync::atomic::Ordering::Acquire) {
            return Err("main ASR worker is not running".to_string());
        }
        if cancel_epoch.load(std::sync::atomic::Ordering::Acquire) != handled_cancel_epoch {
            return Err("recording finalization cancelled".to_string());
        }
        let (reply_tx, reply_rx) = channel();
        self.main_mailbox.push_final(FinalRequest::Dictation {
            generation,
            audio,
            reply: reply_tx,
        });
        Ok(reply_rx)
    }

    /// Second half of `transcribe_final_result`: wait for a queued dictation final.
    pub(crate) fn wait_final_result(
        &self,
        generation: u64,
        reply_rx: Receiver<Result<TranscriptionResult, String>>,
        cancel_epoch: &std::sync::atomic::AtomicU64,
        handled_cancel_epoch: u64,
    ) -> Result<TranscriptionResult, String> {
        let result = wait_for_final_reply(
            reply_rx,
            FINAL_REPLY_TIMEOUT,
            cancel_epoch,
            handled_cancel_epoch,
            "main ASR final",
        );
        if cancel_epoch.load(std::sync::atomic::Ordering::Acquire) != handled_cancel_epoch {
            self.cancel_recording(generation);
        }
        result
    }

    pub(crate) fn transcribe_final_control_result(
        &self,
        generation: u64,
        audio: Vec<f32>,
        onset_sample: usize,
        intent: rightkit_text_polish::text_pipeline::ControlIntent,
        cancel_epoch: &std::sync::atomic::AtomicU64,
        handled_cancel_epoch: u64,
    ) -> Result<FinalControlResult, String> {
        if !self.main_alive.load(std::sync::atomic::Ordering::Acquire) {
            return Err("main ASR worker is not running".to_string());
        }
        if cancel_epoch.load(std::sync::atomic::Ordering::Acquire) != handled_cancel_epoch {
            return Err("recording finalization cancelled".to_string());
        }
        let (reply_tx, reply_rx) = channel();
        self.main_mailbox.push_final(FinalRequest::ZephyrControl {
            generation,
            audio,
            onset_sample,
            intent,
            reply: reply_tx,
        });
        let result = wait_for_final_reply(
            reply_rx,
            FINAL_REPLY_TIMEOUT,
            cancel_epoch,
            handled_cancel_epoch,
            "main ASR final control",
        );
        if cancel_epoch.load(std::sync::atomic::Ordering::Acquire) != handled_cancel_epoch {
            self.cancel_recording(generation);
        }
        result
    }

    /// Handle for a helper thread to queue one file job. Captures the current
    /// fail-all epoch so a `fail_all_file_jobs` issued while the file is still
    /// decoding also fails it once queued.
    pub(crate) fn file_submitter(&self) -> FileSubmitter {
        FileSubmitter {
            mailbox: Arc::clone(&self.main_mailbox),
            main_alive: Arc::clone(&self.main_alive),
            epoch: self.main_mailbox.file_fail_epoch(),
        }
    }

    pub(crate) fn fail_all_file_jobs(&self, code: &str) {
        self.main_mailbox.fail_all_file_jobs(code);
    }

    #[allow(dead_code)]
    pub(crate) fn has_file_work(&self) -> bool {
        self.main_mailbox.has_file_work()
    }

    pub(crate) fn set_dictation_active(&self, active: bool) {
        self.main_mailbox.set_dictation_active(active);
    }

    pub(crate) fn request_keep_warm_burst(&self) {
        self.main_mailbox.request_keep_warm_burst();
    }

    pub(crate) fn set_keep_warm_continuous(&self, on: bool) {
        self.main_mailbox.set_keep_warm_continuous(on);
    }

    #[cfg(test)]
    pub(crate) fn reload_model(&self) -> Result<bool, String> {
        let deadline = Instant::now() + Duration::from_secs(61);
        let _guard = loop {
            match self.reload_lock.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Err("ASR reload lock deadline exceeded".to_string());
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        };
        reload_model_until_stable(
            &self.main_mailbox,
            &self.probe_mailbox,
            &self.main_alive,
            &self.probe_alive,
            &self.control_cleanup_supported,
            deadline,
        )
    }

    pub(crate) fn shutdown(&self) {
        self.clear_pending();
        let _ = self.reload_tx.send(ReloadCoordinatorCommand::Shutdown);
        self.probe_mailbox.push_control(ProbeControl::Shutdown);
        self.main_mailbox.push_control(MainAsrControl::Shutdown);
    }
}

/// Final decoding runs on its model owner; keep the capture coordinator
/// responsive to Cancel even if that owner is still finishing an old decode.
fn wait_for_final_reply<T>(
    reply: Receiver<Result<T, String>>,
    timeout: Duration,
    cancel_epoch: &std::sync::atomic::AtomicU64,
    handled_cancel_epoch: u64,
    label: &str,
) -> Result<T, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancel_epoch.load(std::sync::atomic::Ordering::Acquire) != handled_cancel_epoch {
            return Err("recording finalization cancelled".to_string());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("{label} timed out"));
        }
        match reply.recv_timeout(remaining.min(CAPTURE_POLL)) {
            Ok(result) => return result,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(format!("{label} reply dropped"));
            }
        }
    }
}

fn reload_model_once(
    main_mailbox: &MainAsrMailbox,
    probe_mailbox: &ProbeMailbox,
    main_alive: &std::sync::atomic::AtomicBool,
    probe_alive: &std::sync::atomic::AtomicBool,
    control_cleanup_supported: &std::sync::atomic::AtomicBool,
    deadline: Instant,
) -> Result<(bool, bool), String> {
    if !main_alive.load(std::sync::atomic::Ordering::Acquire)
        || !probe_alive.load(std::sync::atomic::Ordering::Acquire)
    {
        return Err("ASR workers are not running".to_string());
    }
    let (main_reply_tx, main_reply_rx) = channel();
    main_mailbox.push_control(MainAsrControl::Reload {
        reply: main_reply_tx,
    });
    let (main_changed, main_support, main_key) = main_reply_rx
        .recv_timeout(reload_timeout(deadline))
        .map_err(|error| match error {
            RecvTimeoutError::Timeout => "main ASR reload timed out".to_string(),
            RecvTimeoutError::Disconnected => "main ASR reload reply dropped".to_string(),
        })??;
    control_cleanup_supported.store(main_support, std::sync::atomic::Ordering::Release);
    let (probe_reply_tx, probe_reply_rx) = channel();
    probe_mailbox.push_control(ProbeControl::Reload {
        reply: probe_reply_tx,
    });
    let (probe_changed, probe_key) = probe_reply_rx
        .recv_timeout(reload_timeout(deadline))
        .map_err(|error| match error {
            RecvTimeoutError::Timeout => "KWS reload timed out".to_string(),
            RecvTimeoutError::Disconnected => "KWS reload reply dropped".to_string(),
        })??;
    Ok((
        main_changed || probe_changed,
        reload_keys_converged(
            &main_key,
            &probe_key,
            &asr_reload_key(),
            cfg!(not(any(target_os = "macos", target_os = "windows"))),
        ),
    ))
}

fn reload_keys_converged(
    main_key: &str,
    probe_key: &str,
    configured_key: &str,
    probe_tracks_settings: bool,
) -> bool {
    main_key == configured_key && (!probe_tracks_settings || probe_key == configured_key)
}

fn reload_model_until_stable(
    main_mailbox: &MainAsrMailbox,
    probe_mailbox: &ProbeMailbox,
    main_alive: &std::sync::atomic::AtomicBool,
    probe_alive: &std::sync::atomic::AtomicBool,
    control_cleanup_supported: &std::sync::atomic::AtomicBool,
    deadline: Instant,
) -> Result<bool, String> {
    let mut changed = false;
    loop {
        if Instant::now() >= deadline {
            return Err("ASR reload did not converge before deadline".to_string());
        }
        let (pass_changed, stable) = reload_model_once(
            main_mailbox,
            probe_mailbox,
            main_alive,
            probe_alive,
            control_cleanup_supported,
            deadline,
        )?;
        changed |= pass_changed;
        if stable {
            return Ok(changed);
        }
        tracing::info!("ASR reload workers disagreed on configured model; applying latest model");
    }
}

fn reload_timeout(deadline: Instant) -> Duration {
    deadline
        .checked_duration_since(Instant::now())
        .unwrap_or_default()
        .min(Duration::from_secs(30))
}

fn run_reload_coordinator<F>(
    reload_rx: Receiver<ReloadCoordinatorCommand>,
    reload_result_tx: Sender<ReloadCompletion>,
    mut reload: F,
    wait_signal: Option<Arc<WorkerWaitSignal>>,
) where
    F: FnMut() -> Result<bool, String>,
{
    loop {
        let first_id = match reload_rx.recv() {
            Ok(ReloadCoordinatorCommand::Request { id }) => id,
            Ok(ReloadCoordinatorCommand::Shutdown) | Err(_) => {
                tracing::info!(phase = "stopped", "ASR reload coordinator");
                return;
            }
        };
        let mut ids = vec![first_id];
        while let Ok(command) = reload_rx.try_recv() {
            match command {
                ReloadCoordinatorCommand::Request { id } => ids.push(id),
                ReloadCoordinatorCommand::Shutdown => {
                    tracing::info!(phase = "stopped", "ASR reload coordinator");
                    return;
                }
            }
        }
        let started = Instant::now();
        tracing::info!(
            phase = "batch_started",
            first_id,
            count = ids.len(),
            "ASR reload coordinator"
        );
        let result = reload();
        tracing::info!(
            phase = "batch_finished",
            first_id,
            count = ids.len(),
            ok = result.is_ok(),
            changed = result.as_ref().ok().copied(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "ASR reload coordinator"
        );
        for id in ids {
            let _ = reload_result_tx.send(ReloadCompletion {
                id,
                result: result.clone(),
            });
            if let Some(signal) = wait_signal.as_ref() {
                signal.notify();
            }
        }
    }
}

pub(crate) fn trace_probe_lifecycle(
    request: &ProbeRequest,
    phase: &'static str,
    probe_ms: Option<u64>,
    ok: Option<bool>,
) {
    if should_trace_probe_lifecycle(request, phase, ok) {
        emit_command_probe(probe_lifecycle_payload(request, phase, probe_ms, ok));
    }
}

pub(crate) fn should_trace_probe_lifecycle(
    request: &ProbeRequest,
    phase: &'static str,
    ok: Option<bool>,
) -> bool {
    request.diagnostic_sample
        || ok == Some(false)
        || matches!(phase, "pending_replaced" | "pending_cleared")
}

pub(crate) fn probe_lifecycle_payload(
    request: &ProbeRequest,
    phase: &'static str,
    probe_ms: Option<u64>,
    ok: Option<bool>,
) -> serde_json::Value {
    json!({
        "event": "probe_lifecycle",
        "phase": phase,
        "lane": request.lane,
        "session_id": request.session_id,
        "generation": request.generation,
        "kws_stream_generation": request.kws_stream_generation,
        "probe_sequence": request.probe_sequence,
        "diagnostic_sample": request.diagnostic_sample,
        "total_samples": request.total_samples,
        "command_samples": request.audio.len(),
        "command_start": request.command_start,
        "stream_base_sample": request.stream_base_sample,
        "native_origin_sample": request.native_origin_sample,
        // Age since submission. At `inference_started` this is mailbox wait: the
        // single-slot mailbox cannot accumulate, so a growing value here means
        // inference is the bottleneck and older snapshots are being replaced.
        "age_ms": request.created_at.elapsed().as_millis() as u64,
        "probe_ms": probe_ms,
        "ok": ok,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProbeStaleReason {
    RecordingGeneration,
    Session,
    KwsStreamGeneration,
    NativeOrigin,
    Age,
    SampleLag,
}

impl ProbeStaleReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::RecordingGeneration => "recording_generation",
            Self::Session => "session",
            Self::KwsStreamGeneration => "kws_stream_generation",
            Self::NativeOrigin => "native_origin",
            Self::Age => "age",
            Self::SampleLag => "sample_lag",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProbeResultDecision {
    pub(crate) current: bool,
    pub(crate) reset_stream: bool,
    pub(crate) reason: Option<ProbeStaleReason>,
    pub(crate) age_ms: u64,
    pub(crate) live_lag_samples: usize,
}

/// Classify a probe before its text can reach command handling. Retired
/// recording/stream results are rejected without touching current state. A
/// current stream's old empty/prefix result is also only dropped: it carries
/// no KWS latch and resetting it would discard the latest pending snapshot.
/// Native completion/error results do require reset because KWS may already
/// have latched or invalidated its cascade before publishing the result.
pub(crate) fn probe_result_decision(
    result: &ProbeResult,
    generation: u64,
    session_id: Option<&str>,
    kws_stream_generation: u64,
    _native_origin_sample: Option<usize>,
    live_total_samples: usize,
    now: worker_clock::WorkerInstant,
) -> ProbeResultDecision {
    let age = now.duration_since(result.request.submitted_at);
    let age_ms = age.as_millis() as u64;
    let live_lag_samples = live_total_samples.saturating_sub(result.request.total_samples);
    let stale = if result.request.generation != generation {
        Some(ProbeStaleReason::RecordingGeneration)
    } else if session_id != Some(result.request.session_id.as_str()) {
        Some(ProbeStaleReason::Session)
    } else if result.request.kws_stream_generation != kws_stream_generation {
        Some(ProbeStaleReason::KwsStreamGeneration)
    } else if result.native_origin_sample != result.request.command_start {
        Some(ProbeStaleReason::NativeOrigin)
    } else if age > Duration::from_secs(2) {
        Some(ProbeStaleReason::Age)
    } else if live_lag_samples > SAMPLE_RATE as usize * 2 {
        Some(ProbeStaleReason::SampleLag)
    } else {
        None
    };

    let reset_stream = match stale {
        Some(ProbeStaleReason::NativeOrigin) => true,
        Some(ProbeStaleReason::Age | ProbeStaleReason::SampleLag) => {
            result.transcript.is_err()
                || result.native_control_intent.is_some()
                || result
                    .transcript
                    .as_ref()
                    .ok()
                    .and_then(|text| native_control_intent(text))
                    .is_some()
        }
        _ => false,
    };

    ProbeResultDecision {
        current: stale.is_none(),
        reset_stream,
        reason: stale,
        age_ms,
        live_lag_samples,
    }
}

#[cfg(test)]
pub(crate) fn probe_result_is_current(
    result: &ProbeResult,
    generation: u64,
    session_id: Option<&str>,
    kws_stream_generation: u64,
    native_origin_sample: Option<usize>,
    live_total_samples: usize,
    now: worker_clock::WorkerInstant,
) -> bool {
    probe_result_decision(
        result,
        generation,
        session_id,
        kws_stream_generation,
        native_origin_sample,
        live_total_samples,
        now,
    )
    .current
}

pub(crate) fn opening_result_is_current(
    result: &OpeningCommandResult,
    generation: u64,
    session_id: Option<&str>,
) -> bool {
    result.request.generation == generation
        && session_id == Some(result.request.session_id.as_str())
}

pub(crate) fn latch_probe_lane_failure(failed: &mut bool) -> bool {
    if *failed {
        false
    } else {
        *failed = true;
        true
    }
}

#[cfg(test)]
#[path = "unified_probe_lane_tests.rs"]
mod unified_probe_lane_tests;
