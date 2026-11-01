//! Developer-mode streaming insertion: decode the trailing window on the main
//! ASR lane while recording and publish words once they are stable.

#[allow(unused_imports)]
use super::*;

const STREAM_STALL: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(crate) struct StreamLane {
    committer: StreamCommitter,
    in_flight: bool,
    last_submit: Option<Instant>,
}

impl Worker {
    /// Drain finished streaming decodes, then submit the next one when the
    /// lane is idle. Runs every pass while recording.
    pub(super) fn pump_stream(&mut self) {
        let generation = self.recording_generation;
        while let Some(result) = self.asr_executor.try_recv_stream() {
            let (Some(lane), Some(session)) = (self.stream.as_mut(), self.session.as_ref()) else {
                continue;
            };
            if result.generation != generation {
                continue;
            }
            lane.in_flight = false;
            let transcript = match result.result {
                Ok(transcript) => transcript,
                Err(error) => {
                    tracing::warn!(error, "streaming insertion decode failed");
                    continue;
                }
            };
            let offset_sec = result.start_sample as f32 / SAMPLE_RATE as f32;
            let audio_end_sec = result.end_sample as f32 / SAMPLE_RATE as f32;
            if lane
                .committer
                .observe(&transcript.tokens, offset_sec, audio_end_sec)
            {
                let _ = self.event_tx.send(WorkerEvent::StreamCommitted {
                    session_id: session.id.clone(),
                    text: lane.committer.text(),
                    revision: lane.committer.revision(),
                    audio_secs: audio_end_sec,
                });
            }
        }
        let (Some(lane), Some(session)) = (self.stream.as_mut(), self.session.as_ref()) else {
            return;
        };
        // A request dropped before it ran (clear_pending) never answers.
        let stalled = lane
            .last_submit
            .is_some_and(|at| at.elapsed() > STREAM_STALL);
        if (lane.in_flight && !stalled)
            || session.buffer.len() < STREAM_MIN_SAMPLES
            || lane
                .last_submit
                .is_some_and(|at| at.elapsed() < STREAM_INTERVAL)
        {
            return;
        }
        let start_sample = session.buffer.len().saturating_sub(STREAM_WINDOW_SAMPLES);
        let audio = session.buffer[start_sample..].to_vec();
        lane.in_flight = true;
        lane.last_submit = Some(Instant::now());
        self.asr_executor
            .submit_stream(generation, start_sample, audio);
    }
}
