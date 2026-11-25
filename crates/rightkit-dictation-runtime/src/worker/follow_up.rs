use std::collections::VecDeque;
use std::time::{Duration, Instant};

use rightkit_audio::echo::{reference_tap, EchoCanceller};

use super::worker_api::SAMPLE_RATE;

// Voice detection is `rightkit_audio::turn::is_voiced`: model-free on purpose, because the follow-up
// window only decides when to start a recording; the recording itself is transcribed as any other.
/// Voiced audio in a row, in samples, before speech is taken to have begun (125 ms).
const ONSET_SAMPLES: usize = SAMPLE_RATE as usize / 8;
/// Audio kept from before the onset, so the first word is not clipped (500 ms).
const PRE_ROLL_SAMPLES: usize = SAMPLE_RATE as usize / 2;

/// An open session: after a command or a reply the microphone stays open for a window, and speech that
/// begins inside it starts an addressed recording with no wake word. This is why the worker polls
/// instead of parking while it exists (`WakeListener::is_active`). A wake alone submits nothing, and
/// neither does an expired window.
pub(crate) struct FollowUpWatch {
    deadline: Instant,
    pre_roll: VecDeque<f32>,
    voiced_run: usize,
    /// Takes the agent's own voice, as the process is playing it (`reference_tap`), out of what the
    /// microphone hears, so speech over a reply is heard without it. Learns nothing and changes
    /// nothing while nothing is playing.
    echo: EchoCanceller,
}

/// What one chunk of audio did to the window.
pub(crate) enum FollowUpStep {
    Waiting,
    /// Speech began: the audio to seed the recording with.
    Speech(Vec<f32>),
    Expired,
}

impl FollowUpWatch {
    pub(crate) fn open(now: Instant, window: Duration) -> Self {
        Self {
            deadline: now + window,
            pre_roll: VecDeque::with_capacity(PRE_ROLL_SAMPLES + ONSET_SAMPLES),
            voiced_run: 0,
            echo: EchoCanceller::default(),
        }
    }

    pub(crate) fn ingest(&mut self, now: Instant, chunk: &[f32]) -> FollowUpStep {
        if now >= self.deadline {
            return FollowUpStep::Expired;
        }
        let heard =
            self.echo
                .process_with_tap(reference_tap(), crate::host::echo_path_delay(), chunk);
        self.pre_roll.extend(heard.iter().copied());
        let loud = rightkit_audio::turn::is_voiced(&heard);
        if loud {
            self.voiced_run += chunk.len();
        } else {
            self.voiced_run = 0;
            while self.pre_roll.len() > PRE_ROLL_SAMPLES {
                self.pre_roll.pop_front();
            }
        }
        if self.voiced_run >= ONSET_SAMPLES {
            return FollowUpStep::Speech(self.pre_roll.drain(..).collect());
        }
        FollowUpStep::Waiting
    }
}
