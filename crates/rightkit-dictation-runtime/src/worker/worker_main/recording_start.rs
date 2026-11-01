//! StartRecording: retire any previous state, create the session, and bring
//! capture up without losing the first syllable.

#[allow(unused_imports)]
use super::*;

impl Worker {
    pub(super) fn start_recording(&mut self, session_id: String) -> Flow {
        // Pause queued file jobs before any capture/ASR setup: dictation wins.
        self.asr_executor.set_dictation_active(true);
        let wake_handoff = self.wake.start_pending;
        if self.wake.listening {
            self.wake.listening = false;
            self.asr_executor.stop_idle_wake();
            self.capture.pause();
        }
        self.wake.start_pending = false;
        let start_received_at = Instant::now();
        rightkit_dictation_inference::timeline_recorder::session_started();
        trace_recording_start(&session_id, "worker_start_received", 0);
        self.recording_generation = self.recording_generation.wrapping_add(1);
        self.asr_executor.clear_pending();
        loop {
            match self.asr_executor.try_recv() {
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(error) => {
                    if latch_probe_lane_failure(&mut self.asr_executor_failed) {
                        tracing::error!(error, "ASR executor unavailable");
                    }
                    break;
                }
            }
        }
        self.reset_kws_stream();
        self.speech_vad.reset();
        self.session = Some(RecordingSession::new(
            session_id.clone(),
            self.decision_clock.now(),
        ));
        self.reload_ready_generation = None;
        self.stream = crate::settings::streaming_insertion().then(StreamLane::default);
        trace_recording_start(
            &session_id,
            "recording_active",
            start_received_at.elapsed().as_millis() as u64,
        );

        if wake_handoff {
            // The wake path kept the stream running; its audio since the
            // wake word is the start of this recording.
            trace_recording_start(
                &session_id,
                "capture_resumed",
                start_received_at.elapsed().as_millis() as u64,
            );
            let handoff = self.wake.take_handoff();
            self.session
                .as_mut()
                .expect("recording session")
                .buffer
                .extend_from_slice(&handoff);
            trace_recording_start(
                &session_id,
                "wake_audio_handoff",
                start_received_at.elapsed().as_millis() as u64,
            );
            if self.capture.stream.is_none() {
                return self
                    .fail_recording_start(&session_id, "capture stream is not live".to_string());
            }
        } else {
            let saved = crate::settings::input_device();
            let start = match self.capture.prepare_for_recording(
                saved,
                &session_id,
                start_received_at,
                self.decision_clock.as_ref(),
            ) {
                Ok(start) => start,
                Err(message) => return self.fail_recording_start(&session_id, message),
            };
            let session = self.session.as_mut().expect("recording session");
            session.buffer.extend_from_slice(&start.route_prefix);
            if let Some(seed) = start.seed {
                session.ingest(&seed.samples, &mut self.speech_vad, seed.at);
            }
            if let Some(cal) = start.calibration {
                self.speech_vad.calibrate(&cal);
                tracing::info!(
                    "vad auto-calibrated: noise_floor={:.5} speech_threshold={:.4}",
                    self.speech_vad.noise_floor_rms(),
                    self.speech_vad.speech_threshold(),
                );
            }
            if let Some(repaired_at) = start.repaired_at {
                // Share the session's one-repair budget with streaming health.
                session.capture_zero_guard.repair_attempted = true;
                session.speech.last_voice_at = repaired_at;
            }
        }
        // Keep capture live while the fallback model check runs. The reload
        // coordinator is serialized & polled by the worker; any failure is
        // still reported against this exact session generation.
        if let Err(error) = self.queue_reload(None, true) {
            return self.fail_recording_start(
                &session_id,
                format!("ASR reload ({}): {error}", asr_reload_key()),
            );
        }
        let session = self.session.as_mut().expect("recording session");
        session.capture_zero_guard.observe(&session.buffer);
        tracing::info!("worker recording started");
        Flow::Proceed
    }

    pub(super) fn fail_recording_start(&mut self, session_id: &str, message: String) -> Flow {
        let _ = self.retire_session(Decoding::Keep);
        self.capture.pause();
        let _ = self.event_tx.send(WorkerEvent::CaptureFailed {
            session_id: session_id.to_string(),
            message,
        });
        Flow::SkipPass
    }
}
