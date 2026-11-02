//! Results from the opening-command lane (main ASR) and the Zephyr KWS lane.
//! Both only decide; ending the recording is `recording_finish`'s job.

#[allow(unused_imports)]
use super::*;

impl Worker {
    pub(super) fn drain_opening_results(&mut self) {
        loop {
            let opening_result = match self.asr_executor.try_recv_opening() {
                Ok(Some(result)) => result,
                Ok(None) => break,
                Err(error) => {
                    if latch_probe_lane_failure(&mut self.asr_executor_failed) {
                        tracing::error!(error, "main ASR opening lane unavailable");
                    }
                    break;
                }
            };
            self.asr_executor.opening_result_drained();
            if !opening_result_is_current(
                &opening_result,
                self.recording_generation,
                self.session.as_ref().map(|session| session.id.as_str()),
            ) {
                if let Some(session) = self.session.as_mut() {
                    session.pending_opening_action = None;
                }
                trace_command_probe(CommandProbeLog {
                    lane: "opening_command_main_asr",
                    session_id: Some(&opening_result.request.session_id),
                    probe_ms: opening_result.decode_ms,
                    recognized_text: "",
                    classifier_result: "stale_result_dropped",
                    speech_start_sample: opening_result.request.speech_start_sample,
                    command_start: opening_result.request.command_start,
                    command_samples: opening_result.request.audio.len(),
                    total_samples: opening_result.request.total_samples,
                    pending_prefix: opening_result.request.pending_prefix,
                    error: None,
                });
                continue;
            }
            // Current results always belong to the live session.
            let session = self
                .session
                .as_mut()
                .expect("current opening result has a session");

            let text = match opening_result.transcript {
                Ok(text) => text,
                Err(error) => {
                    session.pending_opening_action = None;
                    trace_command_probe(CommandProbeLog {
                        lane: "opening_command_main_asr",
                        session_id: Some(&opening_result.request.session_id),
                        probe_ms: opening_result.decode_ms,
                        recognized_text: "",
                        classifier_result: "probe_error",
                        speech_start_sample: opening_result.request.speech_start_sample,
                        command_start: opening_result.request.command_start,
                        command_samples: opening_result.request.audio.len(),
                        total_samples: opening_result.request.total_samples,
                        pending_prefix: opening_result.request.pending_prefix,
                        error: Some(error.as_str()),
                    });
                    continue;
                }
            };
            let trimmed = text.trim();
            let classification =
                crate::command_classify::classify_streaming(trimmed, crate::settings::is_pro());
            let should_fire = match &classification {
                CommandClassification::Complete(action) => opening_action_confirmation(
                    &mut session.pending_opening_action,
                    &normalized_opening_action_identity(action),
                    opening_result.request.total_samples,
                    opening_result.request.pause_ready,
                    false,
                ),
                CommandClassification::AmbiguousComplete(action) => opening_action_confirmation(
                    &mut session.pending_opening_action,
                    &normalized_opening_action_identity(action),
                    opening_result.request.total_samples,
                    opening_result.request.pause_ready,
                    true,
                ),
                CommandClassification::Prefix | CommandClassification::None => {
                    session.pending_opening_action = None;
                    false
                }
            };
            let classifier_result = match &classification {
                CommandClassification::Complete(_) if should_fire => "complete_confirmed",
                CommandClassification::Complete(_) => "complete_awaiting_confirmation",
                CommandClassification::AmbiguousComplete(_) if should_fire => "ambiguous_confirmed",
                CommandClassification::AmbiguousComplete(_) => "ambiguous_awaiting_confirmation",
                CommandClassification::Prefix => "prefix",
                CommandClassification::None if trimmed.is_empty() => "empty",
                CommandClassification::None => "none",
            };
            trace_command_probe(CommandProbeLog {
                lane: "opening_command_main_asr",
                session_id: Some(&session.id),
                probe_ms: opening_result.decode_ms,
                recognized_text: trimmed,
                classifier_result,
                speech_start_sample: opening_result.request.speech_start_sample,
                command_start: opening_result.request.command_start,
                command_samples: opening_result.request.audio.len(),
                total_samples: opening_result.request.total_samples,
                pending_prefix: opening_result.request.pending_prefix,
                error: None,
            });

            if should_fire {
                let standalone_action = match &classification {
                    CommandClassification::Complete(action)
                    | CommandClassification::AmbiguousComplete(action) => action.clone(),
                    _ => unreachable!("only complete classifications confirm"),
                };
                let ambiguous =
                    matches!(classification, CommandClassification::AmbiguousComplete(_));
                if let Some((session, _)) = self.retire_session(Decoding::Cancel) {
                    self.finish_standalone_command(session, standalone_action, ambiguous, text);
                }
                continue;
            }

            match classification {
                CommandClassification::Complete(_)
                | CommandClassification::AmbiguousComplete(_)
                | CommandClassification::Prefix => {
                    mark_opening_prefix_result_at(
                        &mut session.speech.checked_this_pause,
                        &mut session.pending_command_prefix_since,
                        self.decision_clock.now(),
                    );
                }
                CommandClassification::None if !trimmed.is_empty() => {
                    if pause_can_close_command_probe(
                        session.speech.heard_voice,
                        opening_result.request.pause_ready,
                    ) {
                        session.speech.checked_this_pause = true;
                        session.pending_command_prefix_since = None;
                    }
                }
                CommandClassification::None => {}
            }
        }
    }

    pub(super) fn drain_probe_results(&mut self) {
        loop {
            let probe_result = match self.asr_executor.try_recv() {
                Ok(Some(result)) => result,
                Ok(None) => break,
                Err(error) => {
                    if latch_probe_lane_failure(&mut self.asr_executor_failed) {
                        tracing::error!(error, "ASR executor unavailable");
                    }
                    break;
                }
            };
            self.asr_executor.probe_result_drained();
            #[cfg(test)]
            replay_note_probe_processed(probe_result.request.total_samples, probe_result.probe_ms);
            #[cfg(test)]
            let _dispatch_guard = replay_probe_dispatch_guard(probe_result.request.total_samples);
            // No session means no buffered audio: every path that ends a
            // recording empties or drops its buffer.
            let live_total_samples = self
                .session
                .as_ref()
                .map_or(0, |session| session.buffer.len());
            let decision = probe_result_decision(
                &probe_result,
                self.recording_generation,
                self.session.as_ref().map(|session| session.id.as_str()),
                self.kws_stream_generation,
                self.kws_native_origin_sample,
                live_total_samples,
                self.decision_clock.now(),
            );
            if !decision.current {
                emit_command_probe(json!({
                    "event": "probe_stale_rejection",
                    "lane": probe_result.request.lane,
                    "session_id": &probe_result.request.session_id,
                    "probe_sequence": probe_result.request.probe_sequence,
                    "reason": decision.reason.map(ProbeStaleReason::as_str).unwrap_or("unknown"),
                    "reset_stream": decision.reset_stream,
                    "current_generation": self.recording_generation,
                    "request_generation": probe_result.request.generation,
                    "current_kws_stream_generation": self.kws_stream_generation,
                    "request_kws_stream_generation": probe_result.request.kws_stream_generation,
                    "request_native_origin_sample": probe_result.request.native_origin_sample,
                    "result_native_origin_sample": probe_result.native_origin_sample,
                    "request_total_samples": probe_result.request.total_samples,
                    "live_total_samples": live_total_samples,
                    "age_ms": decision.age_ms,
                    "live_lag_samples": decision.live_lag_samples,
                    "compute_ms": probe_result.probe_ms,
                    "queue_wait_ms": probe_result.queue_wait_ms,
                    "result_wait_ms": Instant::now()
                        .duration_since(probe_result.completed_at)
                        .as_millis() as u64,
                }));
                // Only a result from the current recording and current KWS stream may
                // reset state. A native completion may have latched KWS, while an
                // empty/prefix result is safe to drop and keeps the latest snapshot
                // queued. Results from an already-retired stream must never cascade
                // resets into the fresh stream that replaced it.
                if decision.reset_stream {
                    if let Some(session) = self.session.as_mut() {
                        session.pending_control_prefix_since = None;
                    }
                    self.reset_kws_stream();
                }
                trace_command_probe(CommandProbeLog {
                    lane: probe_result.request.lane,
                    session_id: Some(&probe_result.request.session_id),
                    probe_ms: probe_result.probe_ms,
                    recognized_text: "",
                    classifier_result: "stale_result_dropped",
                    speech_start_sample: probe_result.request.speech_start_sample,
                    command_start: probe_result.request.command_start,
                    command_samples: probe_result.request.audio.len(),
                    total_samples: probe_result.request.total_samples,
                    pending_prefix: probe_result.request.pending_control_prefix,
                    error: None,
                });
                continue;
            }

            let text = match &probe_result.transcript {
                Ok(text) => text.clone(),
                Err(error) => {
                    if let Some(session) = self.session.as_mut() {
                        session.pending_control_prefix_since = None;
                    }
                    self.reset_kws_stream();
                    trace_command_probe(CommandProbeLog {
                        lane: probe_result.request.lane,
                        session_id: Some(&probe_result.request.session_id),
                        probe_ms: probe_result.probe_ms,
                        recognized_text: "",
                        classifier_result: "probe_error",
                        speech_start_sample: probe_result.request.speech_start_sample,
                        command_start: probe_result.request.command_start,
                        command_samples: probe_result.request.audio.len(),
                        total_samples: probe_result.request.total_samples,
                        pending_prefix: probe_result.request.pending_control_prefix,
                        error: Some(error.as_str()),
                    });
                    continue;
                }
            };
            // Current results always belong to the live session.
            let session = self
                .session
                .as_mut()
                .expect("current probe result has a session");
            let trimmed = text.trim();
            // Only the KWS cascade's anchored full-phrase result may authorize STOP, SEND, or
            // CANCEL. Main-ASR text below can still keep Stage 2 armed, but it cannot
            // become an action through exact or fuzzy parsing.
            let control_intent = probe_result.native_control_intent;
            let saw_control_candidate =
                rightkit_text_polish::text_pipeline::has_trailing_control_wake(trimmed)
                    || tail_text_ends_with_control_verb(trimmed);
            if should_keep_control_candidate_armed(control_intent.is_some(), saw_control_candidate)
            {
                session
                    .pending_control_prefix_since
                    .get_or_insert_with(|| self.decision_clock.now());
            } else {
                session.pending_control_prefix_since = None;
            }

            if let Some(intent) = control_intent {
                trace_command_probe(CommandProbeLog {
                    lane: probe_result.request.lane,
                    session_id: Some(&session.id),
                    probe_ms: probe_result.probe_ms,
                    recognized_text: trimmed,
                    classifier_result: "control_complete_kws",
                    speech_start_sample: probe_result.request.speech_start_sample,
                    command_start: probe_result.request.command_start,
                    command_samples: probe_result.request.audio.len(),
                    total_samples: probe_result.request.total_samples,
                    pending_prefix: probe_result.request.pending_control_prefix,
                    error: None,
                });
                if let Some((session, final_generation)) = self.retire_session(Decoding::Keep) {
                    self.finish_zephyr_control(session, intent, &probe_result, final_generation);
                }
                continue;
            }

            if saw_control_candidate || probe_result.request.diagnostic_sample {
                trace_command_probe(CommandProbeLog {
                    lane: probe_result.request.lane,
                    session_id: Some(&session.id),
                    probe_ms: probe_result.probe_ms,
                    recognized_text: trimmed,
                    classifier_result: if saw_control_candidate {
                        "control_prefix"
                    } else {
                        "none"
                    },
                    speech_start_sample: probe_result.request.speech_start_sample,
                    command_start: probe_result.request.command_start,
                    command_samples: probe_result.request.audio.len(),
                    total_samples: probe_result.request.total_samples,
                    pending_prefix: probe_result.request.pending_control_prefix,
                    error: None,
                });
            }
        }
    }
}
