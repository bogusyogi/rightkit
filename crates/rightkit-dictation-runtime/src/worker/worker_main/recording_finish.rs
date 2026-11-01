//! Every way a recording ends. Each path retires the session once through
//! `retire_session` and crosses the stop boundary through
//! `close_capture_at_stop`.

#[allow(unused_imports)]
use super::*;

/// What the stop boundary does with audio still queued in the device ring.
#[derive(Clone, Copy)]
enum StopTail {
    /// Observe through VAD and append (manual stop).
    Observe,
    /// Append unobserved (Zephyr: the cut resolver bounds the utterance).
    Append,
    /// Hand it back unappended (standalone command diagnostics; auto-stop
    /// appends it itself under the duration cap).
    Return,
}

impl Worker {
    /// The stop boundary every finalizing path crosses, in this order:
    /// snapshot the paste destination, drain the device ring, pause capture
    /// (indicator off; the stream stays built), then start the optional AX
    /// prefetch so it overlaps the final decode. Returns the tail for
    /// `StopTail::Return`, otherwise an empty vector.
    fn close_capture_at_stop(
        &mut self,
        session: &mut RecordingSession,
        tail: StopTail,
        prefetch: bool,
    ) -> Vec<f32> {
        self.focus.lock().snapshot_at_stop();
        let mut returned = Vec::new();
        if let Some(cap) = &mut self.capture.stream {
            match tail {
                StopTail::Observe => {
                    if let Ok(chunk) = cap.read_f32(usize::MAX) {
                        session.ingest(&chunk, &mut self.speech_vad, self.decision_clock.now());
                    }
                }
                StopTail::Append => {
                    if let Ok(extra) = cap.read_f32(usize::MAX) {
                        session.buffer.extend_from_slice(&extra);
                    }
                }
                StopTail::Return => returned = cap.read_f32(usize::MAX).unwrap_or_default(),
            }
            cap.pause();
        }
        #[cfg(target_os = "macos")]
        if prefetch {
            self.focus.lock().snapshot_at_stop_prefetch();
        }
        #[cfg(not(target_os = "macos"))]
        let _ = prefetch;
        returned
    }

    /// ENG-06 (macOS, `RIGHTKIT_STOP_SNAPSHOT_AFTER_QUEUE=1`): the second half of the stop boundary,
    /// run after the final decode is queued. Same steps and order as `close_capture_at_stop`
    /// minus the ring drain, which already ran: snapshot, pause, prefetch.
    #[cfg(target_os = "macos")]
    fn close_capture_after_queue(&mut self) {
        let snapshot_started = Instant::now();
        self.focus.lock().snapshot_at_stop();
        let snapshot_ms = snapshot_started.elapsed().as_millis() as u64;
        let pause_started = Instant::now();
        self.capture.pause();
        tracing::info!(
            event = "stop_boundary_after_queue",
            snapshot_ms,
            capture_pause_ms = pause_started.elapsed().as_millis() as u64,
            "stop snapshot and capture pause ran beside the final decode"
        );
        self.focus.lock().snapshot_at_stop_prefetch();
    }

    pub(super) fn stop_recording(&mut self, send_enter: bool) -> Flow {
        rightkit_dictation_inference::timeline_recorder::mark(|t| &mut t.worker_pickup_ms);
        let recording_generation = self.recording_generation;
        let Some((mut session, final_generation)) = self.retire_session(Decoding::Keep) else {
            return Flow::SkipPass;
        };
        // ENG-06: macOS only, test-mode switch, default off. Only when the reload barrier is
        // already clear and a stream exists, so every early exit below still sees today's order.
        #[cfg(target_os = "macos")]
        let final_first = crate::perf_levers::stop_snapshot_after_queue()
            && self.reload_ready_generation == Some(recording_generation)
            && self.capture.stream.is_some();
        #[cfg(not(target_os = "macos"))]
        let final_first = false;
        if final_first {
            // Drain only (the sample cutoff); snapshot, pause and prefetch follow the queue.
            if let Some(cap) = &mut self.capture.stream {
                if let Ok(chunk) = cap.read_f32(usize::MAX) {
                    session.ingest(&chunk, &mut self.speech_vad, self.decision_clock.now());
                }
            }
        } else {
            self.close_capture_at_stop(&mut session, StopTail::Observe, true);
        }
        rightkit_dictation_inference::timeline_recorder::mark(|t| &mut t.audio_sealed_ms);
        let session_id = session.id.clone();
        if let Err(error) = self.wait_for_reload_generation(recording_generation, &session_id) {
            #[cfg(target_os = "macos")]
            if final_first {
                self.close_capture_after_queue();
            }
            let _ = self.event_tx.send(WorkerEvent::TranscriptFinal {
                session_id,
                capture_warning: None,
                result: Err(error),
                audio_secs: Some(audio_secs(&session.buffer)),
                stage_metrics: RecordingStageMetrics::default(),
            });
            return Flow::SkipPass;
        }
        if is_no_microphone_audio_at_stop(
            &session.buffer,
            self.capture
                .stream
                .as_ref()
                .is_some_and(|cap| !cap.is_replay()),
        ) {
            #[cfg(target_os = "macos")]
            if final_first {
                self.close_capture_after_queue();
            }
            // A user can stop before the 1.5 s recovery grace expires.
            // Never misreport that pure-zero input as an ASR failure.
            self.asr_executor.cancel_recording(final_generation);
            self.capture.stream = None;
            let _ = self.event_tx.send(WorkerEvent::TranscriptFinal {
                session_id,
                capture_warning: None,
                result: Err("No microphone audio detected. Check your microphone or mute setting, then try again.".into()),
                audio_secs: None,
                stage_metrics: RecordingStageMetrics::default(),
            });
            return Flow::SkipPass;
        }
        tracing::info!(
            "worker recording stopped; transcribing {} samples ({:.1}s)",
            session.buffer.len(),
            session.buffer.len() as f32 / SAMPLE_RATE as f32
        );
        trace_recording_stop(
            Some(&session_id),
            "manual_stop",
            session.buffer.len(),
            session.speech.heard_voice,
            session.speech.speech_start_sample,
            None,
            send_enter,
        );
        owner_diagnostics::capture_audio_parts(&session_id, &session.buffer, &[]);
        let final_audio_secs = audio_secs(&session.buffer);
        let final_audio = std::mem::take(&mut session.buffer);
        let asr_started = Instant::now();
        #[cfg(target_os = "macos")]
        let transcribed = if final_first {
            // ENG-06: the decode starts now; CGWindowList and the CoreAudio pause overlap it.
            let queued = self.queue_final_result(final_generation, final_audio);
            self.close_capture_after_queue();
            queued.and_then(|reply_rx| self.wait_final_result(final_generation, reply_rx))
        } else {
            self.transcribe_final_result(final_generation, final_audio)
        };
        #[cfg(not(target_os = "macos"))]
        let transcribed = self.transcribe_final_result(final_generation, final_audio);
        let result = transcribed.map(|result| {
            crate::worker::FinalTranscript::from_asr(result, crate::worker::StopEvidence::Manual)
        });
        let asr_elapsed_ms = asr_started.elapsed().as_millis() as u64;
        rightkit_dictation_inference::timeline_recorder::mark(|t| &mut t.asr_done_ms);
        let vad_aggregate = self.speech_vad.recording_aggregate();
        owner_diagnostics::record_event(json!({
            "event": "final_transcript",
            "session_id": &session_id,
            "stop_reason": "manual_stop",
            "raw_transcript": result.as_ref().ok().map(|value| &value.text),
            "error": result.as_ref().err(),
        }));
        let _ = self.event_tx.send(WorkerEvent::TranscriptFinal {
            session_id,
            capture_warning: None,
            result,
            audio_secs: Some(final_audio_secs),
            stage_metrics: RecordingStageMetrics {
                vad_observed_frame_count: vad_aggregate.map(|value| value.0),
                vad_speech_frame_count: vad_aggregate.map(|value| value.1),
                asr_elapsed_ms: Some(asr_elapsed_ms),
                polish_elapsed_ms: None,
            },
        });
        self.reset_kws_stream();
        Flow::Proceed
    }

    pub(super) fn cancel_recording(&mut self) {
        // External Cancel discards: no focus snapshot, no tail, no decode.
        let Some((session, _)) = self.retire_session(Decoding::Cancel) else {
            return;
        };
        self.capture.pause();
        trace_recording_stop(
            Some(&session.id),
            "external_cancel",
            session.buffer.len(),
            session.speech.heard_voice,
            session.speech.speech_start_sample,
            None,
            false,
        );
        self.reset_kws_stream();
        tracing::info!(
            target: "cancel_lifecycle",
            phase = "worker_consumed",
            session_id = session.id.as_str(),
            "worker recording cancelled"
        );
    }

    pub(super) fn finish_standalone_command(
        &mut self,
        mut session: RecordingSession,
        standalone_action: CommandAction,
        ambiguous: bool,
        text: String,
    ) {
        let sid = session.id.clone();
        // The command text is already recognised, so the tail is kept only
        // for diagnostics.
        let diagnostic_tail = self.close_capture_at_stop(&mut session, StopTail::Return, true);
        let _ = self.event_tx.send(WorkerEvent::AutoStopBegin {
            session_id: sid.clone(),
            send_enter: false,
            standalone_action: Some(standalone_action.clone()),
        });
        let stop_reason = if ambiguous {
            "standalone_ambiguous_command"
        } else {
            "standalone_command"
        };
        trace_recording_stop(
            Some(&sid),
            stop_reason,
            session.buffer.len(),
            session.speech.heard_voice,
            session.speech.speech_start_sample,
            None,
            false,
        );
        owner_diagnostics::capture_audio_parts(&sid, &session.buffer, &diagnostic_tail);
        owner_diagnostics::record_event(json!({
            "event": "final_transcript",
            "session_id": &sid,
            "stop_reason": stop_reason,
            "raw_transcript": &text,
            "stripped_transcript": &text,
        }));
        let vad_aggregate = self.speech_vad.recording_aggregate();
        let _ = self.event_tx.send(WorkerEvent::AutoStop {
            session_id: sid,
            result: Ok(crate::worker::FinalTranscript {
                text,
                tokens: Vec::new(),
                stop_evidence: crate::worker::StopEvidence::Standalone {
                    action: standalone_action.clone(),
                },
            }),
            audio_secs: Some(audio_secs(&session.buffer)),
            send_enter: false,
            standalone_action: Some(standalone_action),
            stage_metrics: RecordingStageMetrics {
                vad_observed_frame_count: vad_aggregate.map(|value| value.0),
                vad_speech_frame_count: vad_aggregate.map(|value| value.1),
                asr_elapsed_ms: None,
                polish_elapsed_ms: None,
            },
        });
        session.buffer.clear();
    }

    pub(super) fn finish_zephyr_control(
        &mut self,
        mut session: RecordingSession,
        intent: rightkit_text_polish::text_pipeline::ControlIntent,
        probe_result: &ProbeResult,
        final_generation: u64,
    ) {
        use rightkit_text_polish::text_pipeline::ControlIntent;
        let sid = session.id.clone();
        #[cfg(test)]
        let confirmation_sample = session.buffer.len();
        // The tail is appended unobserved: the cut resolver below decides
        // where the utterance ends. Spoken cancel saves to history and needs
        // no polish-only AX prefetch.
        let is_cancel = matches!(intent, ControlIntent::Cancel);
        self.close_capture_at_stop(&mut session, StopTail::Append, !is_cancel);
        #[cfg(test)]
        if let Some(controller) = replay_controller() {
            let mut state = controller
                .state
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.confirmed_onset_sample = probe_result.trigger_onset_sample;
            state.confirmation_sample = Some(confirmation_sample);
            state.stop_buffer_samples = Some(session.buffer.len());
        }
        if is_cancel {
            // Cancel-to-history: a spoken cancel must still SAVE the
            // utterance (0.1.86 contract), so run the full-buffer decode
            // like Stop instead of discarding. AutoCancelBegin latches
            // the cancel stop kind in the runtime first so the finalize
            // stays L0-only/no-cloud and the shell routes to history.
            let _ = self.event_tx.send(WorkerEvent::AutoCancelBegin {
                session_id: sid.clone(),
            });
            if let Err(error) = self.wait_for_reload_generation(final_generation, &sid) {
                let _ = self.event_tx.send(WorkerEvent::TranscriptFinal {
                    session_id: sid,
                    capture_warning: None,
                    result: Err(error),
                    audio_secs: Some(audio_secs(&session.buffer)),
                    stage_metrics: RecordingStageMetrics::default(),
                });
                return;
            }
            owner_diagnostics::capture_audio_parts(&sid, &session.buffer, &[]);
            trace_recording_stop(
                Some(&sid),
                "zephyr_cancel",
                session.buffer.len(),
                session.speech.heard_voice,
                session.speech.speech_start_sample,
                None,
                false,
            );
            let started = Instant::now();
            let result = self
                .transcribe_final_result(final_generation, session.buffer.clone())
                .map(|mut full| {
                    // Strip a RECOGNIZED trailing cancel tail only; an
                    // unrecognized suffix (e.g. the mis-heard trigger) is
                    // preserved — this text is history-only evidence.
                    let stripped = rightkit_text_polish::text_pipeline::strip_fired_control_tail(
                        &full.text,
                        ControlIntent::Cancel,
                    );
                    if stripped != full.text {
                        full.tokens.clear();
                    }
                    full.text = stripped;
                    crate::worker::FinalTranscript::from_asr(
                        full,
                        crate::worker::StopEvidence::Zephyr {
                            intent: ControlIntent::Cancel,
                            control_start_sample: probe_result.trigger_onset_sample,
                            control_end_sample: None,
                            cut_status: crate::worker::ControlCutStatus::Skipped,
                        },
                    )
                });
            let asr_elapsed_ms = Some(started.elapsed().as_millis() as u64);
            let vad_aggregate = self.speech_vad.recording_aggregate();
            let _ = self.event_tx.send(WorkerEvent::TranscriptFinal {
                session_id: sid,
                capture_warning: None,
                result,
                audio_secs: Some(audio_secs(&session.buffer)),
                stage_metrics: RecordingStageMetrics {
                    vad_observed_frame_count: vad_aggregate.map(|value| value.0),
                    vad_speech_frame_count: vad_aggregate.map(|value| value.1),
                    asr_elapsed_ms,
                    polish_elapsed_ms: None,
                },
            });
            return;
        }
        let send_enter = matches!(intent, ControlIntent::Send);
        let _ = self.event_tx.send(WorkerEvent::AutoStopBegin {
            session_id: sid.clone(),
            send_enter,
            standalone_action: None,
        });
        if let Err(error) = self.wait_for_reload_generation(final_generation, &sid) {
            let _ = self.event_tx.send(WorkerEvent::AutoStop {
                session_id: sid,
                result: Err(error),
                audio_secs: Some(audio_secs(&session.buffer)),
                send_enter,
                standalone_action: None,
                stage_metrics: RecordingStageMetrics::default(),
            });
            return;
        }
        let stop_reason = if send_enter {
            "zephyr_send"
        } else {
            "zephyr_stop"
        };
        let control_start_sample = probe_result.trigger_onset_sample;
        trace_recording_stop(
            Some(&sid),
            stop_reason,
            session.buffer.len(),
            session.speech.heard_voice,
            session.speech.speech_start_sample,
            None,
            send_enter,
        );
        // Tested macOS TDT keeps the complete recording for one normal final
        // decode, then removes only the confirmed control in text/timing. The
        // executor owns model capability and runs cleanup on its model thread;
        // missing onset deliberately stays on the legacy cut route.
        if should_use_cleanup_first_control(
            self.asr_executor.supports_control_cleanup(),
            intent,
            probe_result.native_control_intent,
            control_start_sample,
        ) {
            if let Some(onset_sample) = control_start_sample {
                let started = Instant::now();
                let cleanup = self.transcribe_final_control_result(
                    final_generation,
                    session.buffer.clone(),
                    onset_sample,
                    intent,
                );
                let asr_elapsed = started.elapsed().as_millis() as u64;
                let asr_elapsed_ms = Some(asr_elapsed);
                let (
                    result,
                    original_text,
                    boundary_sec,
                    method,
                    score,
                    joint_calls,
                    cleanup_error,
                ) = match cleanup {
                    Ok(outcome) => {
                        let original_text = outcome.original_text.clone();
                        tracing::info!(
                            branch = "cleanup_first",
                            duration_ms = asr_elapsed,
                            boundary_sec = ?outcome.boundary_sec,
                            method = outcome.method,
                            score = ?outcome.score,
                            joint_calls = outcome.joint_calls,
                            error = ?outcome.error,
                            "zephyr control cleanup complete"
                        );
                        (
                            Ok(crate::worker::FinalTranscript::from_asr(
                                outcome.transcript,
                                crate::worker::StopEvidence::Zephyr {
                                    intent,
                                    control_start_sample: Some(onset_sample),
                                    control_end_sample: None,
                                    cut_status: crate::worker::ControlCutStatus::Skipped,
                                },
                            )),
                            Some(original_text),
                            outcome.boundary_sec,
                            Some(outcome.method),
                            outcome.score,
                            outcome.joint_calls,
                            outcome.error,
                        )
                    }
                    Err(error) => {
                        tracing::warn!(
                            branch = "cleanup_first",
                            duration_ms = asr_elapsed,
                            error = %error,
                            "zephyr control cleanup failed"
                        );
                        (Err(error), None, None, None, None, 0, None)
                    }
                };
                owner_diagnostics::capture_audio_parts(&sid, &session.buffer, &[]);
                owner_diagnostics::record_event(json!({
                    "event": "final_transcript",
                    "session_id": &sid,
                    "stop_reason": stop_reason,
                    "raw_transcript": original_text,
                    "stripped_transcript": result.as_ref().ok().map(|value| &value.text),
                    "cleanup_method": method,
                    "cleanup_boundary_sec": boundary_sec,
                    "cleanup_score": score,
                    "cleanup_joint_calls": joint_calls,
                    "cleanup_error": cleanup_error,
                }));
                let vad_aggregate = self.speech_vad.recording_aggregate();
                let _ = self.event_tx.send(WorkerEvent::AutoStop {
                    session_id: sid,
                    result,
                    audio_secs: Some(audio_secs(&session.buffer)),
                    send_enter,
                    standalone_action: None,
                    stage_metrics: RecordingStageMetrics {
                        vad_observed_frame_count: vad_aggregate.map(|value| value.0),
                        vad_speech_frame_count: vad_aggregate.map(|value| value.1),
                        asr_elapsed_ms,
                        polish_elapsed_ms: None,
                    },
                });
                return;
            }
        }
        // Sherpa bounds a confidence-gated Static15 seam. Its token
        // timestamp is not an audio cut authority: if no safe seam
        // resolves, preserve full audio & clean confirmed residue later.
        let trigger_cut = resolve_trigger_audio_cut(
            probe_result.trigger_onset_sample,
            probe_result.trigger_onset_skip_reason,
            session.speech.speech_start_sample,
            &session.buffer,
        );
        let kws_onset_cut = matches!(
            &trigger_cut,
            TriggerAudioCut::Cut { cut_source, .. }
                if *cut_source == rightkit_dictation_inference::KWS_ONSET_CUT_SOURCE
        );
        let (decode_end, mut cut_status) = match trigger_cut {
            TriggerAudioCut::Cut {
                onset_sample,
                cut_at,
                cut_source,
                seam_rms,
                noise_floor_rms,
            } => {
                trace_trigger_audio_cut(
                    &sid,
                    onset_sample,
                    cut_at,
                    cut_source,
                    seam_rms,
                    noise_floor_rms,
                    session.buffer.len(),
                );
                (cut_at, crate::worker::ControlCutStatus::Cut)
            }
            TriggerAudioCut::FullBuffer { reason } => {
                trace_trigger_audio_cut_fallback(Some(&sid), reason, session.buffer.len());
                (
                    session.buffer.len(),
                    crate::worker::ControlCutStatus::Skipped,
                )
            }
        };
        let started = Instant::now();
        let mut result = if decode_end == 0 {
            Ok(rightkit_dictation_inference::TranscriptionResult {
                text: String::new(),
                tokens: Vec::new(),
            })
        } else {
            self.transcribe_final_result(final_generation, session.buffer[..decode_end].to_vec())
        };
        if result.is_err() && decode_end < session.buffer.len() {
            trace_trigger_audio_cut_fallback(
                Some(&sid),
                "cut_decode_failed_full_buffer_retry",
                session.buffer.len(),
            );
            cut_status = crate::worker::ControlCutStatus::Skipped;
            result = self.transcribe_final_result(final_generation, session.buffer.clone());
        }
        let asr_elapsed_ms = if decode_end == 0 {
            None
        } else {
            Some(started.elapsed().as_millis() as u64)
        };
        // The retry above decodes the full buffer, so the onset-cut
        // residue rule applies only while the onset cut still stands.
        let kws_onset_cut =
            kws_onset_cut && matches!(cut_status, crate::worker::ControlCutStatus::Cut);
        let result = result.map(|mut full| {
            let residue_stripped = if kws_onset_cut {
                rightkit_text_polish::text_pipeline::strip_kws_onset_cut_residue(&full.text)
            } else {
                full.text.clone()
            };
            let stripped = rightkit_text_polish::text_pipeline::strip_fired_control_tail(
                &residue_stripped,
                intent,
            );
            owner_diagnostics::record_event(json!({
                "event": "final_transcript",
                "session_id": &sid,
                "stop_reason": stop_reason,
                "raw_transcript": &full.text,
                "stripped_transcript": &stripped,
            }));
            tracing::info!(
                raw = command_log_text(full.text.trim()),
                stripped = command_log_text(stripped.trim()),
                "zephyr tail: strip"
            );
            // A defensive textual strip changes token/text alignment.
            // Treat that rare lane as untimed rather than letting stale
            // control tokens influence semantic transform routing.
            if stripped != full.text {
                full.tokens.clear();
                full.text = stripped;
            }
            crate::worker::FinalTranscript::from_asr(
                full,
                crate::worker::StopEvidence::Zephyr {
                    intent,
                    control_start_sample,
                    control_end_sample: None,
                    cut_status,
                },
            )
        });
        owner_diagnostics::capture_audio_parts(&sid, &session.buffer, &[]);
        let vad_aggregate = self.speech_vad.recording_aggregate();
        let _ = self.event_tx.send(WorkerEvent::AutoStop {
            session_id: sid,
            result,
            audio_secs: Some(audio_secs(&session.buffer)),
            send_enter,
            standalone_action: None,
            stage_metrics: RecordingStageMetrics {
                vad_observed_frame_count: vad_aggregate.map(|value| value.0),
                vad_speech_frame_count: vad_aggregate.map(|value| value.1),
                asr_elapsed_ms,
                polish_elapsed_ms: None,
            },
        });
    }

    /// Post-speech silence, the duration cap, or a failed zero-capture
    /// recovery ends the recording and transcribes it.
    pub(super) fn finish_auto_stop(
        &mut self,
        mut session: RecordingSession,
        final_generation: u64,
        duration_cap_hit: bool,
    ) {
        let stop_reason: &'static str = if session.capture_zero_failure.is_some() {
            "capture_zero_recovery_failed"
        } else if duration_cap_hit {
            "max_duration_cap"
        } else {
            "post_speech_silence"
        };
        if duration_cap_hit {
            tracing::warn!(
                total_samples = session.buffer.len(),
                max_seconds = MAX_RECORDING_SECONDS,
                "recording duration cap reached: auto-stopping and transcribing recording"
            );
        } else {
            tracing::info!("post-speech silence: auto-stopping and transcribing recording");
        }
        let sid = session.id.clone();
        let diagnostic_tail = self.close_capture_at_stop(&mut session, StopTail::Return, true);
        // Capture ownership does not end at silence decision: audio
        // already queued in device ring is part of this interaction.
        // Append it before final decode; hard cap remains exact.
        if duration_cap_hit {
            let remaining = MAX_RECORDING_SAMPLES.saturating_sub(session.buffer.len());
            session
                .buffer
                .extend_from_slice(&diagnostic_tail[..diagnostic_tail.len().min(remaining)]);
        } else {
            session.buffer.extend_from_slice(&diagnostic_tail);
        }
        trace_recording_stop(
            Some(&sid),
            stop_reason,
            session.buffer.len(),
            session.speech.heard_voice,
            session.speech.speech_start_sample,
            Some(
                self.decision_clock
                    .now()
                    .duration_since(session.speech.last_voice_at)
                    .as_millis() as u64,
            ),
            false,
        );
        owner_diagnostics::capture_audio_parts(&sid, &session.buffer, &diagnostic_tail);
        if session.capture_zero_failure.is_some() {
            let _ = self.event_tx.send(WorkerEvent::AutoCancelBegin {
                session_id: sid.clone(),
            });
        } else {
            let _ = self.event_tx.send(WorkerEvent::AutoStopBegin {
                session_id: sid.clone(),
                send_enter: false,
                standalone_action: None,
            });
        }
        if let Err(error) = self.wait_for_reload_generation(final_generation, &sid) {
            let result = Err(error);
            let final_event = if session.capture_zero_failure.is_some() {
                WorkerEvent::TranscriptFinal {
                    session_id: sid,
                    result,
                    capture_warning: session.capture_zero_failure.take(),
                    audio_secs: Some(audio_secs(&session.buffer)),
                    stage_metrics: RecordingStageMetrics::default(),
                }
            } else {
                WorkerEvent::AutoStop {
                    session_id: sid,
                    result,
                    audio_secs: Some(audio_secs(&session.buffer)),
                    send_enter: false,
                    standalone_action: None,
                    stage_metrics: RecordingStageMetrics::default(),
                }
            };
            let _ = self.event_tx.send(final_event);
            return;
        }
        let final_audio_secs = audio_secs(&session.buffer);
        let final_audio = std::mem::take(&mut session.buffer);
        let asr_started = Instant::now();
        let result = self
            .transcribe_final_result(final_generation, final_audio)
            .map(|result| {
                crate::worker::FinalTranscript::from_asr(
                    result,
                    if duration_cap_hit {
                        crate::worker::StopEvidence::Duration
                    } else {
                        crate::worker::StopEvidence::Silence
                    },
                )
            });
        let asr_elapsed_ms = asr_started.elapsed().as_millis() as u64;
        let vad_aggregate = self.speech_vad.recording_aggregate();
        owner_diagnostics::record_event(json!({
            "event": "final_transcript",
            "session_id": &sid,
            "stop_reason": stop_reason,
            "raw_transcript": result.as_ref().ok().map(|value| &value.text),
            "error": result.as_ref().err(),
        }));
        let stage_metrics = RecordingStageMetrics {
            vad_observed_frame_count: vad_aggregate.map(|value| value.0),
            vad_speech_frame_count: vad_aggregate.map(|value| value.1),
            asr_elapsed_ms: Some(asr_elapsed_ms),
            polish_elapsed_ms: None,
        };
        let final_event = if session.capture_zero_failure.is_some() {
            WorkerEvent::TranscriptFinal {
                session_id: sid,
                result,
                capture_warning: session.capture_zero_failure.take(),
                audio_secs: Some(final_audio_secs),
                stage_metrics,
            }
        } else {
            WorkerEvent::AutoStop {
                session_id: sid,
                result,
                audio_secs: Some(final_audio_secs),
                send_enter: false,
                standalone_action: None,
                stage_metrics,
            }
        };
        let _ = self.event_tx.send(final_event);
    }
}
