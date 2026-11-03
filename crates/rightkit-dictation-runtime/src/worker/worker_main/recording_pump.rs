//! The per-pass audio work while recording: read and validate capture,
//! repair a zero-only stream, submit probes, and run the silence and
//! duration backstops.

#[allow(unused_imports)]
use super::*;

impl Worker {
    pub(super) fn pump_recording(&mut self) {
        let mut audio_arrived_this_loop = false;
        let read = self.capture.stream.as_mut().map(|cap| {
            cap.wait_for_audio(CAPTURE_POLL);
            (cap.read_f32(usize::MAX), cap.is_replay())
        });
        match read {
            Some((Ok(chunk), is_replay)) if !chunk.is_empty() => {
                let session = self
                    .session
                    .as_mut()
                    .expect("pump_recording needs a session");
                let previous_for_validation = if is_replay {
                    None
                } else {
                    session.previous_capture_block.as_ref()
                };
                let validation = match validate_capture_pcm(&chunk, previous_for_validation) {
                    Ok(validation) => validation,
                    Err(error) => {
                        tracing::error!(error, "capture ingress rejected");
                        self.fail_live_capture(error.into());
                        return;
                    }
                };
                let discarded_repeat = validation.discarded_repeat();
                session.previous_capture_block = Some(validation.history());
                if discarded_repeat {
                    tracing::warn!("capture ingress discarded one repeated block");
                    return;
                }
                session.capture_zero_guard.observe(&chunk);
                session.voice_ema = session.voice_ema * 0.6 + chunk_level(&chunk) * 0.4;
                let silence_burst =
                    session.ingest(&chunk, &mut self.speech_vad, self.decision_clock.now());
                if silence_burst {
                    self.asr_executor.request_keep_warm_burst();
                }
                if session.last_level_emit.elapsed() >= Duration::from_millis(100) {
                    session.last_level_emit = Instant::now();
                    let _ = self.event_tx.send(WorkerEvent::RecordingLevel {
                        session_id: session.id.clone(),
                        level: session.voice_ema.clamp(0.0, 1.0),
                    });
                }
                audio_arrived_this_loop = true;
            }
            Some((Err(e), _)) => {
                // The cpal callback records only lock-free state. Snapshot it
                // on the worker thread when a read fails so terminal errors
                // retain their typed cause while benign async warnings remain
                // nonterminal and do not trigger an inline reopen.
                let health = self
                    .capture
                    .stream
                    .as_ref()
                    .map(WorkerCapture::metrics_snapshot);
                tracing::error!(
                    error = %e,
                    async_error_kind = health
                        .as_ref()
                        .and_then(|health| health.last_async_error_kind)
                        .map(|kind| kind.as_str())
                        .unwrap_or("none"),
                    terminal_error_kind = health
                        .as_ref()
                        .and_then(|health| health.terminal_error_kind)
                        .map(|kind| kind.as_str())
                        .unwrap_or("none"),
                    async_errors = health
                        .as_ref()
                        .map_or(0, |health| health.async_error_count),
                    "capture read error"
                );
                self.fail_live_capture(format!("capture read: {e}"));
                return;
            }
            Some((Ok(_), _)) | None => {}
        }
        // Async callback errors arrive outside this worker pass. Observe the
        // atomic snapshot here so post-seed warnings are visible without
        // logging from the realtime callback or on every pump iteration.
        self.capture.log_new_async_errors();

        // macOS can resume a stale CoreAudio stream with callbacks carrying
        // only zeros. Detect throughout recording, including its first seed.
        // Windows endpoint recovery remains owned by its lifecycle adapter.
        let needs_zero_recovery = self
            .session
            .as_ref()
            .is_some_and(|session| session.capture_zero_guard.needs_recovery());
        if cfg!(target_os = "macos")
            && needs_zero_recovery
            && self
                .capture
                .stream
                .as_ref()
                .is_some_and(|cap| !cap.is_replay())
            && !self.recover_zero_capture(&mut audio_arrived_this_loop)
        {
            return;
        }

        self.run_recording_decisions(audio_arrived_this_loop);
    }

    /// The live stream failed mid-recording: drop it (the next start reopens)
    /// and discard the recording.
    fn fail_live_capture(&mut self, message: String) {
        self.capture.pause();
        self.capture.stream = None;
        if let Some((session, _)) = self.retire_session(Decoding::Cancel) {
            let _ = self.event_tx.send(WorkerEvent::CaptureFailed {
                session_id: session.id,
                message,
            });
        }
        self.reset_kws_stream();
    }

    /// One reopen for a stream delivering only digital zeros. Returns false
    /// when the recording ended.
    fn recover_zero_capture(&mut self, audio_arrived_this_loop: &mut bool) -> bool {
        let saved = crate::settings::input_device();
        let session = self
            .session
            .as_mut()
            .expect("zero recovery needs a session");
        let recovery = session
            .capture_zero_guard
            .begin_repair()
            .map_err(str::to_string)
            .and_then(|()| {
                reopen_zero_capture(
                    &mut self.capture.stream,
                    &mut self.capture.route,
                    saved.as_deref(),
                    self.decision_clock.as_ref(),
                )
            });
        match recovery {
            Ok(seed) => {
                self.capture.saved_device = saved;
                session.previous_capture_block = None;
                session.capture_zero_guard.observe(&seed);
                session.ingest(&seed, &mut self.speech_vad, self.decision_clock.now());
                *audio_arrived_this_loop = true;
                tracing::info!(target: "capture_health", phase = "streaming",
                    outcome = if seed.iter().any(|sample| *sample != 0.0) {
                        "recovered"
                    } else { "reopened_awaiting_audio" },
                    repair_attempt = 1u8, "capture health transition");
                true
            }
            Err(error) => {
                tracing::error!(target: "capture_health", phase = "streaming",
                    outcome = "capture_zero_recovery_failed", repair_attempt = 1u8,
                    %error, "capture health transition");
                self.capture.pause();
                self.capture.stream = None;
                session.previous_capture_block = None;
                session.capture_zero_failure = Some(CAPTURE_NO_AUDIO_MESSAGE.to_string());
                // Nothing useful exists to transcribe when every sample
                // was zero. A nonzero prefix follows history-only finalize
                // in the auto-stop pass, retaining speech without an
                // unsolicited paste.
                if session.buffer.iter().any(|sample| *sample != 0.0) {
                    return true;
                }
                let (session, _) = self
                    .retire_session(Decoding::Cancel)
                    .expect("zero recovery needs a session");
                // Use terminal finalize even if a concurrent Stop
                // already moved runtime into Transcribing.
                let _ = self.event_tx.send(WorkerEvent::AutoCancelBegin {
                    session_id: session.id.clone(),
                });
                let _ = self.event_tx.send(WorkerEvent::TranscriptFinal {
                    session_id: session.id,
                    result: Err(CAPTURE_NO_AUDIO_MESSAGE.to_string()),
                    capture_warning: None,
                    audio_secs: None,
                    stage_metrics: RecordingStageMetrics::default(),
                });
                false
            }
        }
    }

    /// Prefix timeouts, probe submission, background segments, and the
    /// silence/duration backstops for the live recording.
    fn run_recording_decisions(&mut self, audio_arrived_this_loop: bool) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let decision_now = self.decision_clock.now();

        if session
            .pending_control_prefix_since
            .is_some_and(|since| tail_prefix_expired(decision_now.duration_since(since)))
        {
            session.pending_control_prefix_since = None;
        }
        if session.pending_command_prefix_since.is_some_and(|since| {
            decision_now.duration_since(since) > Duration::from_millis(CMD_PREFIX_GRACE_MS)
        }) {
            trace_command_probe(CommandProbeLog {
                lane: "unified_command_trigger",
                session_id: Some(&session.id),
                probe_ms: 0,
                recognized_text: "",
                classifier_result: "prefix_timeout",
                speech_start_sample: session.speech.speech_start_sample,
                command_start: command_probe_start_sample(
                    session.speech.speech_start_sample,
                    session.buffer.len(),
                )
                .min(session.buffer.len()),
                command_samples: 0,
                total_samples: session.buffer.len(),
                pending_prefix: true,
                error: None,
            });
            session.pending_command_prefix_since = None;
            session.speech.checked_this_pause = true;
        }

        let vad_ready = self.speech_vad.is_ready();
        let command_start =
            command_probe_start_sample(session.speech.speech_start_sample, session.buffer.len())
                .min(session.buffer.len());
        let command_audio_len = session.buffer.len().saturating_sub(command_start);
        let pause_ready = vad_ready
            && decision_now.duration_since(session.speech.last_voice_at)
                >= Duration::from_millis(PAUSE_GATE_MS);
        // Opening-command ASR owns only the first two seconds of captured
        // audio. Once that bounded window is complete, hand off irreversibly
        // after its queued/active/result state is drained so quiet starts do
        // not strand KWS behind a never-heard VAD pause.
        if session.opening_phase == OpeningPhase::Probing
            && (session.buffer.len() >= SAMPLE_RATE as usize * 2
                || decision_now.duration_since(session.opening_started_at)
                    >= Duration::from_secs(2))
        {
            // Close opening eligibility at the bounded deadline first. Any
            // queued/active/result-pending opening work drains before KWS.
            session.opening_phase = OpeningPhase::Draining;
        }
        if session.opening_phase == OpeningPhase::Draining && self.asr_executor.opening_lane_clear()
        {
            session.opening_phase = OpeningPhase::Kws;
        }
        let opening_due = session.opening_phase == OpeningPhase::Probing
            && !session.speech.checked_this_pause
            && opening_command_due_at(
                session.speech.heard_voice,
                session.pending_command_prefix_since.is_some(),
                pause_ready,
                command_audio_len,
                session.last_command_probe_samples,
                session.last_cmd_check,
                decision_now,
            );
        let pending_control_prefix = session.pending_control_prefix_since.is_some();
        let tail_due = tail_probe_due(session.buffer.len(), audio_arrived_this_loop);
        let reload_ready = self.reload_ready_generation == Some(self.recording_generation);

        if !session.auto_stop_fired
            && session.capture_zero_failure.is_none()
            && !self.asr_executor_failed
            && reload_ready
            && !session.buffer.is_empty()
            && (tail_due || opening_due)
            && probe_gate_clear(&self.cmd_rx, &mut self.pending_cmd_peek)
        {
            if opening_due {
                let opening_submitted = self.asr_executor.submit_opening(OpeningCommandRequest {
                    generation: self.recording_generation,
                    session_id: session.id.clone(),
                    created_at: Instant::now(),
                    audio: session.buffer[command_start..].to_vec(),
                    speech_start_sample: session.speech.speech_start_sample,
                    command_start,
                    total_samples: session.buffer.len(),
                    pause_ready,
                    pending_prefix: session.pending_command_prefix_since.is_some()
                        || session.pending_control_prefix_since.is_some(),
                });
                if opening_submitted {
                    session.last_cmd_check = Some(decision_now);
                    session.last_command_probe_samples =
                        session.buffer.len().saturating_sub(command_start);
                    mark_pause_probe_submitted(&mut session.speech.checked_this_pause, pause_ready);
                }
            }

            if tail_due && session.opening_phase == OpeningPhase::Kws {
                let use_full_tail = should_run_full_tail_fallback(
                    "",
                    pending_control_prefix,
                    session
                        .buffer
                        .len()
                        .saturating_sub(session.last_full_tail_probe_samples),
                );

                let tail_lane = if use_full_tail {
                    TailProbeLane::Full
                } else {
                    TailProbeLane::Fast
                };
                let snapshot_start = tail_probe_start_sample(session.buffer.len(), tail_lane);
                let lane = if use_full_tail {
                    "zephyr_tail_full_async"
                } else {
                    "zephyr_tail_async"
                };

                if use_full_tail {
                    session.last_full_tail_probe_samples = session.buffer.len();
                }
                let probe_sequence = session.kws_probe_count.wrapping_add(1);
                let native_origin_sample = snapshot_start;
                if self.asr_executor.submit_probe(ProbeRequest {
                    generation: self.recording_generation,
                    kws_stream_generation: self.kws_stream_generation,
                    session_id: session.id.clone(),
                    submitted_at: decision_now,
                    created_at: Instant::now(),
                    audio: session.buffer[snapshot_start..].to_vec(),
                    speech_start_sample: session.speech.speech_start_sample,
                    command_start: snapshot_start,
                    stream_base_sample: snapshot_start,
                    native_origin_sample,
                    total_samples: session.buffer.len(),
                    pending_control_prefix,
                    lane,
                    probe_sequence,
                    diagnostic_sample: probe_sequence == 1
                        || probe_sequence.is_multiple_of(KWS_DIAGNOSTIC_SAMPLE_EVERY_PROBES),
                }) {
                    session.kws_probe_count = probe_sequence;
                    self.kws_native_origin_sample = Some(native_origin_sample);
                }
            }
        }

        if reload_ready
            && session.capture_zero_failure.is_none()
            && self.asr_executor.scheduled_static15_enabled()
        {
            if let Some(segment) = scheduled_static15_ready_segment(
                &session.buffer,
                session.scheduled_submitted_samples,
            ) {
                let start_sample = segment.start;
                let end_sample = segment.end;
                self.asr_executor.submit_background(
                    self.recording_generation,
                    start_sample,
                    end_sample,
                    session.buffer[segment].to_vec(),
                );
                session.scheduled_submitted_samples = end_sample;
            }
        }

        if vad_ready
            && session.capture_zero_failure.is_none()
            && !session.auto_stop_fired
            && silence_decision(
                session.speech.heard_voice,
                decision_now.duration_since(session.speech.last_voice_at),
            ) == SilenceDecision::DiscardInitialSilence
        {
            tracing::warn!("initial-silence backstop: discarding recording as a false fire");
            let (session, _) = self
                .retire_session(Decoding::Cancel)
                .expect("recording session");
            self.capture.pause();
            trace_recording_stop(
                Some(&session.id),
                "initial_silence_discard",
                session.buffer.len(),
                session.speech.heard_voice,
                session.speech.speech_start_sample,
                Some(
                    decision_now
                        .duration_since(session.speech.last_voice_at)
                        .as_millis() as u64,
                ),
                false,
            );
            let _ = self.event_tx.send(WorkerEvent::RunawayDiscard {
                session_id: session.id,
            });
            return;
        }

        let duration_cap_hit = recording_duration_cap_exceeded(session.buffer.len());
        let zero_failure = session.capture_zero_failure.is_some();
        if (vad_ready || zero_failure)
            && !session.auto_stop_fired
            && (silence_decision(
                session.speech.heard_voice,
                decision_now.duration_since(session.speech.last_voice_at),
            ) == SilenceDecision::StopAfterPostSpeechSilence
                || duration_cap_hit
                || zero_failure)
        {
            // A silence decision is provisional until device-ring tail is
            // consumed. A renewed word may already be queued behind the
            // last callback that VAD observed; keep this interaction live
            // if that tail re-opens speech.
            if !duration_cap_hit && !zero_failure {
                if let Some(cap) = &mut self.capture.stream {
                    if let Ok(tail) = cap.read_f32(usize::MAX) {
                        if !tail.is_empty() {
                            session.ingest(&tail, &mut self.speech_vad, self.decision_clock.now());
                        }
                    }
                }
                if silence_decision(
                    session.speech.heard_voice,
                    self.decision_clock
                        .now()
                        .duration_since(session.speech.last_voice_at),
                ) != SilenceDecision::StopAfterPostSpeechSilence
                {
                    return;
                }
            }
            let (session, final_generation) = self
                .retire_session(Decoding::Keep)
                .expect("recording session");
            self.finish_auto_stop(session, final_generation, duration_cap_hit);
        }
    }
}
