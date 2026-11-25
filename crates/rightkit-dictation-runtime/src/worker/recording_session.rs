#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{
    capture_recovery::*, probe_decisions::*, unified_probe_lane::*, worker_api::*, worker_clock::*,
};

/// Voice evidence for one recording. `checked_this_pause` lives here because
/// every observed chunk can reopen it; see
/// `observe_voice_chunk_with_terminal_silence`.
pub(crate) struct SpeechTracker {
    pub(crate) last_voice_at: worker_clock::WorkerInstant,
    pub(crate) heard_voice: bool,
    pub(crate) checked_this_pause: bool,
    pub(crate) speech_start_sample: Option<usize>,
    pub(crate) terminal_silences: TerminalSilenceTracker,
}

impl SpeechTracker {
    fn new(now: worker_clock::WorkerInstant) -> Self {
        Self {
            last_voice_at: now,
            heard_voice: false,
            checked_this_pause: false,
            speech_start_sample: None,
            terminal_silences: TerminalSilenceTracker::default(),
        }
    }
}

/// Everything that belongs to one recording. Created fresh by StartRecording
/// and dropped by whichever path ends the recording, so no field needs a
/// hand-written reset. Worker-lifetime state (generations, the VAD model, the
/// capture stream) stays on `Worker`; results that outlive a recording are
/// recognised as stale through those generations.
pub(crate) struct RecordingSession {
    pub(crate) id: String,
    /// The recording began from the wake word (set at the wake hand-off), so its words are addressed to
    /// the agent. A recording from the dictation key never has it.
    pub(crate) addressed: bool,
    /// End-of-turn tracking for an addressed recording, when the host turned turn-taking on
    /// (`host::configure_turn_taking`). It replaces the fixed post-speech silence for this recording.
    pub(crate) turn: Option<rightkit_audio::turn::TurnTracker>,
    /// What `turn` last decided; `None` while there is no tracker.
    pub(crate) turn_decision: Option<rightkit_audio::turn::TurnDecision>,
    pub(crate) buffer: Vec<f32>,
    pub(crate) speech: SpeechTracker,
    pub(crate) previous_capture_block: Option<CapturePcmHistory>,
    pub(crate) capture_zero_guard: CaptureZeroGuard,
    pub(crate) capture_zero_failure: Option<String>,
    pub(crate) scheduled_submitted_samples: usize,
    pub(crate) voice_ema: f32,
    pub(crate) last_level_emit: Instant,
    /// A session auto-stops at most once.
    pub(crate) auto_stop_fired: bool,
    /// An incomplete opening-command prefix reopens the pause check only after
    /// fresh audio arrives, so the decoder can settle.
    pub(crate) pending_command_prefix_since: Option<worker_clock::WorkerInstant>,
    pub(crate) pending_control_prefix_since: Option<worker_clock::WorkerInstant>,
    /// Every standalone action needs a second opening probe with the same
    /// normalized action before it can stop recording.
    pub(crate) pending_opening_action: Option<PendingOpeningAction>,
    pub(crate) last_cmd_check: Option<worker_clock::WorkerInstant>,
    pub(crate) last_command_probe_samples: usize,
    pub(crate) kws_probe_count: u64,
    pub(crate) last_full_tail_probe_samples: usize,
    pub(crate) opening_phase: OpeningPhase,
    pub(crate) opening_started_at: worker_clock::WorkerInstant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OpeningPhase {
    Probing,
    Draining,
    Kws,
}

impl RecordingSession {
    pub(crate) fn new(id: String, now: worker_clock::WorkerInstant) -> Self {
        Self {
            id,
            addressed: false,
            turn: None,
            turn_decision: None,
            buffer: Vec::with_capacity(SAMPLE_RATE as usize * 30),
            speech: SpeechTracker::new(now),
            previous_capture_block: None,
            capture_zero_guard: CaptureZeroGuard::default(),
            capture_zero_failure: None,
            scheduled_submitted_samples: 0,
            voice_ema: 0.0,
            last_level_emit: Instant::now() - Duration::from_millis(120),
            auto_stop_fired: false,
            pending_command_prefix_since: None,
            pending_control_prefix_since: None,
            pending_opening_action: None,
            last_cmd_check: None,
            last_command_probe_samples: 0,
            kws_probe_count: 0,
            last_full_tail_probe_samples: 0,
            opening_phase: OpeningPhase::Probing,
            opening_started_at: now,
        }
    }

    /// Run VAD and terminal-silence bookkeeping over `chunk`, then append it.
    /// Returns true when this chunk takes a silence run past the keep-warm
    /// burst threshold (once per run).
    pub(crate) fn ingest(
        &mut self,
        chunk: &[f32],
        vad: &mut crate::vad::SpeechVad,
        now: worker_clock::WorkerInstant,
    ) -> bool {
        let keep_warm_burst = observe_voice_chunk_with_terminal_silence(
            chunk,
            &mut self.speech.last_voice_at,
            now,
            &mut self.speech.heard_voice,
            &mut self.speech.checked_this_pause,
            vad,
            &mut self.speech.speech_start_sample,
            self.buffer.len(),
            &mut self.speech.terminal_silences,
        );
        if let Some(turn) = self.turn.as_mut() {
            self.turn_decision = Some(turn.push(chunk, rightkit_audio::turn::is_voiced(chunk)));
        }
        self.buffer.extend_from_slice(chunk);
        keep_warm_burst
    }

    /// Whether the post-speech stop rule says this recording is over: the turn tracker's decision when
    /// there is one, else the fixed post-speech silence.
    pub(crate) fn post_speech_stop(&self, now: worker_clock::WorkerInstant) -> bool {
        match self.turn_decision {
            Some(decision) => decision == rightkit_audio::turn::TurnDecision::Ended,
            None => {
                silence_decision(
                    self.speech.heard_voice,
                    now.duration_since(self.speech.last_voice_at),
                ) == SilenceDecision::StopAfterPostSpeechSilence
            }
        }
    }
}
