// Bounded wait for the FINAL transcript of one dictation session (#159).
//
// The shell sends the Stop frame fire-and-forget and the transcript returns as
// an async event, so the only way a wedged finalize shows up is silence. This
// tracker records when each session started recording and when its Stop was
// sent, and answers one question for the shell watchdog: has the audio-length
// scaled budget for this session's final reply run out? Mirrors the file
// transcription bound (`sidecar_file_transcribe_timeout`): a floor, a scaled
// term, and a hard ceiling.

use std::time::{Duration, Instant};

/// Floor: loose enough that cold model/ANE startup cannot look like a wedge.
pub const FINAL_TRANSCRIPT_TIMEOUT_MIN: Duration = Duration::from_secs(60);
/// Ceiling: even an hours-long capture cannot hold the pill past this.
pub const FINAL_TRANSCRIPT_TIMEOUT_MAX: Duration = Duration::from_secs(900);

/// Budget for the final reply: floor + one second of wait per second of audio,
/// clamped to the ceiling.
pub fn final_transcript_timeout(audio: Duration) -> Duration {
    FINAL_TRANSCRIPT_TIMEOUT_MIN
        .saturating_add(audio)
        .min(FINAL_TRANSCRIPT_TIMEOUT_MAX)
}

#[derive(Debug, Default)]
pub(crate) struct FinalWaitTracker {
    recording: Option<(String, Instant)>,
    waiting: Option<FinalWait>,
}

#[derive(Debug, Clone)]
struct FinalWait {
    session_id: String,
    deadline: Instant,
}

impl FinalWaitTracker {
    pub(crate) fn note_recording_started(&mut self, session_id: &str, at: Instant) {
        self.recording = Some((session_id.to_string(), at));
    }

    /// Arm the deadline when Stop is sent. Audio length is the wall-clock span
    /// since recording started (an upper bound on captured audio).
    pub(crate) fn arm(&mut self, session_id: &str, now: Instant) {
        let audio = match &self.recording {
            Some((id, started)) if id == session_id => now.saturating_duration_since(*started),
            _ => Duration::ZERO,
        };
        self.recording = None;
        self.waiting = Some(FinalWait {
            session_id: session_id.to_string(),
            deadline: now + final_transcript_timeout(audio),
        });
    }

    pub(crate) fn clear(&mut self, session_id: &str) {
        if self
            .waiting
            .as_ref()
            .is_some_and(|w| w.session_id == session_id)
        {
            self.waiting = None;
        }
        if self
            .recording
            .as_ref()
            .is_some_and(|(id, _)| id == session_id)
        {
            self.recording = None;
        }
    }

    pub(crate) fn clear_all(&mut self) {
        self.waiting = None;
        self.recording = None;
    }

    pub(crate) fn armed(&self, session_id: &str) -> bool {
        self.waiting
            .as_ref()
            .is_some_and(|w| w.session_id == session_id)
    }

    /// True only when this session has an armed wait whose deadline has passed.
    pub(crate) fn expired(&self, session_id: &str, now: Instant) -> bool {
        self.waiting
            .as_ref()
            .is_some_and(|w| w.session_id == session_id && now >= w.deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_scales_with_audio_and_clamps() {
        assert_eq!(
            final_transcript_timeout(Duration::ZERO),
            FINAL_TRANSCRIPT_TIMEOUT_MIN
        );
        assert_eq!(
            final_transcript_timeout(Duration::from_secs(100)),
            Duration::from_secs(160)
        );
        assert_eq!(
            final_transcript_timeout(Duration::from_secs(10_000)),
            FINAL_TRANSCRIPT_TIMEOUT_MAX
        );
    }

    #[test]
    fn long_recording_gets_longer_budget_than_short() {
        let t0 = Instant::now();
        let mut tracker = FinalWaitTracker::default();
        tracker.note_recording_started("long", t0);
        let stop = t0 + Duration::from_secs(300);
        tracker.arm("long", stop);
        // 60 + 300 = 360 s after stop
        assert!(!tracker.expired("long", stop + Duration::from_secs(359)));
        assert!(tracker.expired("long", stop + Duration::from_secs(360)));
    }

    #[test]
    fn expiry_is_per_session_and_clearable() {
        let t0 = Instant::now();
        let mut tracker = FinalWaitTracker::default();
        tracker.arm("a", t0);
        let late = t0 + FINAL_TRANSCRIPT_TIMEOUT_MIN;
        assert!(tracker.expired("a", late));
        assert!(!tracker.expired("b", late));
        tracker.clear("a");
        assert!(!tracker.expired("a", late));
    }

    #[test]
    fn unarmed_or_other_session_recording_uses_floor() {
        let t0 = Instant::now();
        let mut tracker = FinalWaitTracker::default();
        tracker.note_recording_started("other", t0);
        tracker.arm("a", t0 + Duration::from_secs(500));
        assert!(tracker.expired(
            "a",
            t0 + Duration::from_secs(500) + FINAL_TRANSCRIPT_TIMEOUT_MIN
        ));
    }
}
