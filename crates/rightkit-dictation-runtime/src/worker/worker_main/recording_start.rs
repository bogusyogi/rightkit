//! StartRecording: retire any previous state, create the session, and bring
//! capture up without losing the first syllable.

#[allow(unused_imports)]
use super::*;

impl Worker {
    pub(super) fn start_recording(
        &mut self,
        session_id: String,
        start_attempt: Option<u64>,
    ) -> Flow {
        // The shell explicitly revoked this press (a cancel or stop before its session existed,
        // or a failed press) and the revoke overtook this start in the queue: do not record. This
        // is checked before anything is touched, so wake listening, a live warm-up of another
        // press and the stream stay exactly as they were. Only an explicit revoke fences a start;
        // a warm-up that merely expired still cold starts below. The engine already accepted the
        // start (its runtime is `Recording`), so report it as a failed capture: that unwinds the
        // engine's state, and the shell, which cancelled, drops the stale event.
        if self.capture.is_revoked(start_attempt) {
            tracing::info!(
                session_id = %session_id,
                attempt = ?start_attempt,
                "start_recording_fenced_by_revoked_attempt"
            );
            let _ = self.event_tx.send(WorkerEvent::CaptureFailed {
                session_id,
                message: "start cancelled before recording".to_string(),
            });
            return Flow::SkipPass;
        }
        // Pause queued file jobs before any capture/ASR setup: dictation wins.
        self.asr_executor.set_dictation_active(true);
        let wake_handoff = self.wake.start_pending;
        if self.wake.listening {
            self.wake.listening = false;
            self.asr_executor.stop_idle_wake();
            self.capture.pause();
        }
        self.wake.start_pending = false;
        // A previous recording's release bookkeeping never carries into this one; the wake
        // handoff below has no capture origin, so a release cannot cut it.
        self.session_attempt = start_attempt;
        self.session_capture_origin = None;
        self.session_sealed = false;
        self.session_seal_provenance = None;
        self.session_seal_failed = false;
        self.session_exact_failed = false;
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
        // Attempt-bearing native starts require the original release edge before delivery.
        // Release IPC can arrive arbitrarily late, so direct insertion cannot publish earlier.
        // Deliberate legacy starts (no attempt id) retain developer streaming insertion.
        self.stream = (start_attempt.is_none() && crate::settings::streaming_insertion())
            .then(StreamLane::default);
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
            // The wake stream is this start's capture; close the press so no late warm-up of it
            // can resume anything.
            self.capture.close_attempt(start_attempt);
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
                start_attempt,
            ) {
                Ok(start) => start,
                Err(message) => return self.fail_recording_start(&session_id, message),
            };
            // The release of this press that reached the worker before this start ran, and when
            // this recording's audio began: the manual stop seals the audio at the release.
            self.session_attempt = start_attempt;
            self.session_capture_origin = start.origin;
            let session = self.session.as_mut().expect("recording session");
            session.buffer.extend_from_slice(&start.route_prefix);
            if let Some(seed) = start.seed {
                session.ingest(&seed.samples, &mut self.speech_vad, seed.at);
            }
            if let Some(repaired_at) = start.repaired_at {
                // Share the session's one-repair budget with streaming health.
                session.capture_zero_guard.repair_attempted = true;
                session.speech.last_voice_at = repaired_at;
            }
        }
        // The user released the shortcut of this press before this start ran (a deferred stop,
        // serial hook dispatch): the seal the shell sent was held. Apply it now, so the recording
        // ends at the release, then waits for its stop. A release at or before this recording's
        // capture origin (it was held through a warm-up expiry, or the shell sent it before any
        // capture existed) keeps zero samples: nothing of the clip was spoken before the release.
        // A failed drain leaves the clip unsealed; the stop finalizes it without a cut.
        match self.capture.take_release(start_attempt, &session_id) {
            Ok(Some(released_at)) => {
                if !self.seal_session(released_at) && self.session_exact_failed {
                    return Flow::SkipPass;
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.reject_exact_seal(error);
                return Flow::SkipPass;
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
