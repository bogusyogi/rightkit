// Shared imports live on the module root (runtime.rs); `use super::*` keeps
// this file on the same namespace the include!-based layout provided.
#[allow(unused_imports)]
use super::*;

fn resolved_start_target(
    target: &crate::delivery::TargetSnapshot,
) -> Option<crate::delivery::TargetSnapshot> {
    crate::host::host().resolve_start_target(target)
}

impl EngineRuntime {
    pub fn ensure_history_loaded(&mut self) -> Result<(), String> {
        let current_generation = history_storage_generation();
        if self.history.is_none() || self.history_generation_seen != current_generation {
            // Adopt the store pre-opened outside runtime_lock (lock order: see runtime_lock.rs);
            // open inline only when the prewarm failed, was skipped, or went stale.
            let history = match crate::runtime_lock::take_prewarmed_history(current_generation) {
                Some(store) => store,
                None => build_history_store()?,
            };
            self.recent_cache = history.recent().to_vec();
            self.history = Some(history);
            self.history_generation_seen = current_generation;
        }
        Ok(())
    }

    /// Store a session's terminal record and supersede its crash-recovery
    /// draft in one step (`HistoryStore::finalize_session`): the draft is
    /// removed only once the final record is stored.
    fn persist_session_record(
        &mut self,
        session_id: &str,
        record: DeliveryRecord,
    ) -> Result<(), String> {
        self.ensure_history_loaded()?;
        if let Some(history) = self.history.as_mut() {
            history
                .finalize_session(
                    &rightkit_transcript_library::delivery::draft_delivery_id(session_id),
                    record,
                )
                .map_err(|err| err.to_string())?;
            self.recent_cache = history.recent().to_vec();
        }
        // Final row is durable before this O(1) wake; scheduler performs all
        // network work outside runtime_lock.
        crate::cloud_sync::notify_local_change();
        Ok(())
    }

    /// Drop the frozen stop destination and per-stop state when a session
    /// ends: stop target, focus pin (and the macOS AX observation behind it),
    /// pending stop context and the pending Enter.
    fn release_stop_boundary(&mut self) {
        self.stop_target = None;
        self.stop_focus_pin = None;
        crate::host::host().release_pending_accessibility_observation();
        self.pending_stop_context.clear();
        self.pending_send_enter = false;
    }

    pub fn recent_history(&mut self) -> Result<&[DeliveryRecord], String> {
        self.ensure_history_loaded()?;
        Ok(&self.recent_cache)
    }

    pub fn prepare_start(
        &mut self,
        initial_target: crate::delivery::TargetSnapshot,
        _observer: impl FnMut(&AppState),
    ) -> Result<PreparedStart, String> {
        // F1 fix (shell state-machine audit): a genuinely in-flight backend
        // session — Recording, Transcribing, or the brief Pasting window inside
        // `accept_sidecar_delivery` — always carries a non-None `active_session`.
        // This used to unconditionally force `self.state` back to `Armed` and
        // then overwrite `active_session` with the NEW session id below; the
        // ORIGINAL session's terminal event (delivery/error/reset) would then be
        // dropped as stale by the `active_session` guard in every
        // `accept_sidecar_*` handler, silently losing whatever the user had just
        // dictated. Refuse instead of clobbering — mirrors the engine's own new
        // "reject StartDictation unless Idle/Armed" contract, so a genuinely
        // busy backend and a genuinely busy shell report the identical
        // user-visible error rather than the shell racing ahead of it.
        if self.active_session.is_some() {
            return Err("still finishing the previous dictation".to_string());
        }
        // The engine starts nothing on its own: its Pro flag, vocabulary and AI-polish policy are
        // pushed by us. A running engine gets any change now (the byte-identical skip keeps this
        // cheap); one that has to be (re)spawned for this start gets the same frames from
        // `PreparedStart::send`, before it records. Either way a failure stops the start visibly
        // instead of recording as Free.
        self.invalidate_sidecar_mirrors_if_generation_changed();
        if self.sidecar.supervisor.is_running() {
            self.sync_sidecar_runtime_snapshot()?;
        }
        let snapshot_frames = self.sidecar_snapshot_frames()?;
        let generation_seen = self.sidecar.supervisor.generation();
        // No backend session in flight — Armed, or a completed Pasted /
        // CopiedFallback / Error state that hasn't yet seen a frontend-driven
        // `ResetToArmed` — so it's safe to fold back to Armed and start fresh.
        if !matches!(self.state.get(), AppState::Armed) {
            self.state.set(AppState::Armed);
        }
        self.sequence = self.sequence.saturating_add(1);
        let session_id = format!("session-{}", self.sequence);
        // Drop any start capture a previous session left behind (reset, error
        // and watchdog ends used not to clear it). Otherwise a late detached
        // capture for THIS session leaves the old `start_context` in place and
        // stop-time delivery policy is chosen from another recording's editor.
        self.clear_start_capture();
        let sidecar = &self.sidecar;
        let request_id = format!("start:{}", sidecar.supervisor.next_request_id());
        let mut frame = engine_supervisor::build_request(
            &request_id,
            EngineSchemaName::RecordingStarted,
            Some(session_id.clone()),
            Some(EnginePayload::RecordingStarted {
                session_id: session_id.clone(),
                // Voice undo and the casing commands check the text the shell actually pasted;
                // the engine never sees it otherwise, and a respawned engine never saw it at all.
                last_pasted: self
                    .last_delivery
                    .as_ref()
                    .and_then(rightkit_dictation_core::engine::last_pasted_for_engine),
            }),
        );
        // On macOS, start deliberately does no window-server/AX/UIA work.
        // `None` is an unresolved start identity, never an empty proof.
        let frozen_target = resolved_start_target(&initial_target);
        frame.recording_target = frozen_target.clone();
        if crate::host::host().supports_start_capture() {
            let (tx, rx) = std::sync::mpsc::channel();
            let supervisor = Arc::clone(&sidecar.supervisor);
            let captured_session = session_id.clone();
            let capture_frozen_target = frozen_target.clone();
            let dispatch_ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let dispatch_ready_for_capture = dispatch_ready.clone();
            let _ = std::thread::Builder::new()
                .name("start-context-capture".to_string())
                .spawn(move || {
                    // If no hotkey-time target was available, this sample is
                    // explicitly late. It may enrich diagnostics/context, but
                    // the platform capture routines cannot turn it into an
                    // original-editor binding without a start continuity proof.
                    let (target, pin, context, fingerprint) = crate::host::host()
                        .capture_start_destination_late(capture_frozen_target.clone());
                    let request_id = format!("context:{}", supervisor.next_request_id());
                    let frame = engine_supervisor::build_request(
                        &request_id,
                        EngineSchemaName::RecordingContextCaptured,
                        Some(captured_session.clone()),
                        Some(EnginePayload::RecordingContextCaptured {
                            session_id: captured_session.clone(),
                            context: context.clone(),
                            // A deferred sample is never serialized as a
                            // hotkey-time target. The sidecar must keep its
                            // start identity unresolved until stop delivery.
                            recording_target: capture_frozen_target.clone(),
                        }),
                    );
                    // Detached thread: outwait a slow StartDictation round trip
                    // rather than silently dropping captured context (was 500 ms).
                    for _ in 0..300 {
                        if dispatch_ready_for_capture.load(std::sync::atomic::Ordering::Acquire) {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    if !dispatch_ready_for_capture.load(std::sync::atomic::Ordering::Acquire) {
                        let _ = tx.send((target, pin, context, fingerprint));
                        return;
                    }
                    let _ = supervisor.send(&frame);
                    let _ = tx.send((target, pin, context, fingerprint));
                });
            // Preserve original proof even if short recordings finish before
            // background context capture. It never grants context binding.
            self.start_target = frozen_target.clone();
            self.start_capture_rx = Some(rx);
            self.start_dispatch_ready = Some(dispatch_ready);
        }
        // Claim this session before unlocking; transition gate serializes the
        // matching backend RPC before any later cancel can run.
        self.active_session = Some(session_id.clone());
        self.sync_sidecar_active_session();
        Ok(PreparedStart::with_command(PreparedEngineStart {
            session_id,
            supervisor: Arc::clone(&sidecar.supervisor),
            frame,
            snapshot_frames,
            generation_seen,
        }))
    }

    pub fn finalize_prepared_start(
        &mut self,
        session_id: String,
        mut observer: impl FnMut(&AppState),
    ) -> Result<AppState, String> {
        if self.active_session.as_deref() != Some(session_id.as_str()) {
            return Ok(self.state.get());
        }
        self.apply(
            AppEvent::StartRecording {
                session_id: session_id.clone(),
            },
            &mut observer,
        )?;
        self.active_session = Some(session_id);
        self.sync_sidecar_active_session();
        if let Some(ready) = self.start_dispatch_ready.as_ref() {
            ready.store(true, std::sync::atomic::Ordering::Release);
        }
        self.stop_target = None;
        self.stop_focus_pin = None;
        Ok(self.state.get())
    }

    pub fn fail_prepared_start(
        &mut self,
        session_id: &str,
        message: String,
        mut observer: impl FnMut(&AppState),
    ) -> AppState {
        if self.active_session.as_deref() == Some(session_id) {
            self.active_session = None;
            self.sync_sidecar_active_session();
            self.clear_start_capture();
            let _ = self.apply(AppEvent::Fail { message }, &mut observer);
        }
        self.state.get()
    }

    /// Freeze first stop boundary. Missing identity remains missing; delivery
    /// must never acquire a replacement destination after transcription.
    pub fn capture_stop_destination(&mut self) {
        self.drain_start_capture();
        if self.stop_target.is_some() {
            return;
        }
        let (target, pin) = crate::host::host().capture_stop_destination();
        self.stop_target = Some(target);
        self.stop_focus_pin = pin;
    }

    fn drain_start_capture(&mut self) {
        if let Some(rx) = self.start_capture_rx.take() {
            if let Ok((target, pin, context, fingerprint)) = rx.try_recv() {
                let context_state = context.state;
                // A target sampled after recording began is metadata only. It
                // cannot become an original destination without a hotkey-time
                // identity, even when its later editor read succeeds.
                if self.start_target.is_some() {
                    self.start_target = Some(target);
                }
                self.start_context = Some(context);
                self.start_focus_pin = pin;
                self.start_content_fingerprint = fingerprint;
                crate::telemetry::log_message(
                    "recording_start_context_captured",
                    &format!(
                        "state={:?} pin={} fingerprint={}",
                        context_state,
                        self.start_focus_pin.is_some(),
                        self.start_content_fingerprint.is_some()
                    ),
                );
            } else {
                self.start_capture_rx = Some(rx);
            }
        }
    }

    /// Front half of stop — NON-BLOCKING. Transition to
    /// `transcribing` (pill shows processing), signal the worker (which runs the
    /// final transcribe on its OWN thread and emits delivery/reset/error events
    /// after sidecar finalization), arm the watchdog, and RETURN. Neither the
    /// UI thread nor the global `runtime_lock` is held across the multi-second
    /// transcribe. Idempotent: a 2nd stop, or a stop when not recording, no-ops.
    pub fn prepare_stop(
        &mut self,
        send_enter: bool,
        origin: StopOrigin,
        overlay_handles: Vec<isize>,
        mut observer: impl FnMut(&AppState),
    ) -> Result<PreparedStop, String> {
        crate::telemetry::utterance_timeline::mark(|t| &mut t.gate_acquired_ms);
        let session_id = match (&self.state.get(), self.active_session.clone()) {
            (AppState::Recording { .. }, Some(id)) => id,
            // Already transcribing / armed → idempotent no-op (guards the
            // hotkey+click double-stop race; the in-flight stop will finalize).
            _ => return Ok(PreparedStop::without_command(self.state.get())),
        };
        self.apply(AppEvent::StopRecording, &mut observer)?;
        self.pending_send_enter = send_enter;
        self.pending_stop_context
            .latch_first(PendingStopContext::new(
                session_id.clone(),
                origin,
                overlay_handles,
            ));
        let command = self.prepare_stop_backend_recording(&session_id, send_enter);
        Ok(PreparedStop::with_command(self.state.get(), command, true))
    }

    /// Cancel (any source) finalize-to-history — mirrors `prepare_stop` EXACTLY
    /// (same idempotent guard, same `AppEvent::StopRecording` transition, same
    /// `pending_stop_context` latch, same non-blocking "transition then
    /// return") except: it latches `cancel_to_history_session` FIRST (so
    /// `accept_sidecar_transcript` can recognize this session's finalize even
    /// if the frame round-trips faster than expected) and the engine frame it
    /// builds carries `StopKind::CancelToHistory`, never `Stop`/`SendEnter`.
    /// `send_enter` is always false. Reusing this exact shape is deliberate:
    /// it is the just-deadlock-fixed path, and cancel must not regress it.
    pub fn prepare_cancel_to_history(
        &mut self,
        mut observer: impl FnMut(&AppState),
    ) -> Result<PreparedStop, String> {
        let session_id = match (&self.state.get(), self.active_session.clone()) {
            (AppState::Recording { .. }, Some(id)) => id,
            // Already transcribing / armed → idempotent no-op, exactly like
            // `prepare_stop` (guards a double-cancel / cancel-after-stop race).
            _ => return Ok(PreparedStop::without_command(self.state.get())),
        };
        self.cancel_to_history_session = Some(session_id.clone());
        self.apply(AppEvent::StopRecording, &mut observer)?;
        self.pending_send_enter = false;
        self.pending_stop_context
            .latch_first(PendingStopContext::new(
                session_id.clone(),
                StopOrigin::Generic,
                Vec::new(),
            ));
        let command = self.prepare_cancel_to_history_backend_recording(&session_id);
        Ok(PreparedStop::with_command(self.state.get(), command, false))
    }

    pub fn fail_prepared_stop(
        &mut self,
        session_id: &str,
        message: String,
        mut observer: impl FnMut(&AppState),
    ) -> AppState {
        if self.active_session.as_deref() != Some(session_id) {
            return self.state.get();
        }
        self.active_session = None;
        self.sync_sidecar_active_session();
        self.clear_start_capture();
        if self.cancel_to_history_session.as_deref() == Some(session_id) {
            self.cancel_to_history_session = None;
            self.draft_throttle = None;
            self.pending_stop_context.clear();
            self.pending_send_enter = false;
            self.state.set(AppState::Cancelled {
                transcript: String::new(),
            });
            observer(&self.state.get());
            return self.state.get();
        }
        self.draft_throttle = None;
        self.pending_stop_context.clear();
        self.pending_send_enter = false;
        let _ = self.apply(AppEvent::Fail { message }, &mut observer);
        self.state.get()
    }

    /// Crash-recovery draft history rows: throttled create-or-replace of the
    /// in-progress draft row for `session_id`. Called from the shell's
    /// `TranscriptPartial` event handler, which is NOT under the delivery
    /// lock path (see `accept_sidecar_delivery`/`accept_sidecar_transcript`)
    /// — this method just does a cheap DB upsert and returns; the shell
    /// acquires `runtime_lock` briefly around the call, mirroring every
    /// other per-event handler, and never holds it across the write.
    ///
    /// Throttled to at most once per `DRAFT_UPSERT_MIN_INTERVAL` per session,
    /// and only when `revision` increased, so a live dictation's steady
    /// stream of partials doesn't hammer the encrypted DB. A new session
    /// (session_id mismatch against the throttle state) always writes
    /// immediately — the first partial of a recording must not wait out the
    /// interval before anything is recoverable.
    pub fn upsert_transcript_draft(
        &mut self,
        session_id: String,
        text: String,
        revision: u64,
    ) -> Result<(), String> {
        if self.active_session.as_deref() != Some(session_id.as_str()) {
            return Ok(());
        }
        let now = Instant::now();
        let should_write = match &self.draft_throttle {
            Some((last_session, last_revision, last_at)) if last_session == &session_id => {
                revision > *last_revision
                    && now.duration_since(*last_at) >= DRAFT_UPSERT_MIN_INTERVAL
            }
            _ => true,
        };
        if !should_write {
            return Ok(());
        }
        self.ensure_history_loaded()?;
        if let Some(history) = self.history.as_mut() {
            history
                .upsert_draft(&session_id, text)
                .map_err(|err| err.to_string())?;
            self.recent_cache = history.recent().to_vec();
        }
        self.draft_throttle = Some((session_id, revision, now));
        Ok(())
    }

    pub fn accept_sidecar_delivery(
        &mut self,
        session_id: String,
        record: DeliveryRecord,
        send_enter: bool,
        mut observer: impl FnMut(&AppState),
    ) -> Result<AppState, String> {
        let send_enter = delivery_enter_was_submitted(&record, send_enter);
        if self.active_session.as_deref() != Some(session_id.as_str()) {
            crate::telemetry::log_message(
                "sidecar_delivery_dropped_stale",
                &format!("session={session_id} active={:?}", self.active_session),
            );
            return Ok(self.state.get());
        }
        // Detached start capture may complete after the stop boundary. Drain
        // it again immediately before policy selection; never recapture focus.
        self.drain_start_capture();
        if record.transcript.trim().is_empty() {
            self.state.set(AppState::CopiedFallback {
                delivery_id: record.delivery_id.clone(),
                transcript: record.transcript.clone(),
                reason: CopyFallbackReason::EmptyTranscript,
                send_enter,
            });
            observer(&self.state.get());
            self.active_session = None;
            self.sync_sidecar_active_session();
            self.clear_start_capture();
            self.release_stop_boundary();
            return Ok(self.state.get());
        }
        // Hands-free auto-stop (engine-side zephyr tail / standalone command)
        // finalizes WITHOUT the shell ever driving prepare_stop, so the shell can
        // still be in `Recording` here. `TranscriptFinal` is only valid from
        // `Transcribing` (state.rs), so a raw apply would error via `?` — the paste
        // already happened but the pill stayed stuck "recording". Bridge the
        // transition first (mirrors the engine's prepare_stop-before-finalize); no-op
        // when the shell already drove the stop (already Transcribing).
        if matches!(self.state.get(), AppState::Recording { .. }) {
            self.apply(AppEvent::StopRecording, &mut observer)?;
        }
        self.apply(
            AppEvent::TranscriptFinal {
                transcript: record.transcript.clone(),
                send_enter,
            },
            &mut observer,
        )?;
        self.apply(
            delivery_outcome_event_with_send(&record, send_enter),
            &mut observer,
        )?;
        self.active_session = None;
        self.sync_sidecar_active_session();
        self.clear_start_capture();
        self.release_stop_boundary();
        self.last_delivery = Some(record.clone());
        // The delivery already happened and its outcome state is set above; a
        // history write failure must not turn that into an error. The draft
        // row survives a failed write (see `persist_session_record`).
        if let Err(err) = self.persist_session_record(&session_id, record) {
            crate::telemetry::log_message(
                "history_finalize_failed",
                &format!("session={session_id} path=delivery error={err}"),
            );
        }
        self.draft_throttle = None;
        if let Some(record) = self.last_delivery.as_ref() {
            crate::telemetry::utterance_timeline::note_delivery(record);
            crate::telemetry::log_delivery(record, Some(&session_id), Some(send_enter));
        }
        Ok(self.state.get())
    }

    /// Hands-free auto-stop pre-signal: the sidecar recognized a control tail and is
    /// running the final transcribe. Move Recording→Transcribing NOW so the pill shows
    /// "processing" immediately, instead of sitting in Recording for the whole ~1s
    /// decode. Idempotent + session-guarded; the transcript arrives later via
    /// `accept_sidecar_transcript`.
    ///
    /// Destination parity: the SHELL owns the stop destination for
    /// a hands-free stop exactly as it does for a keyboard/hotkey stop, and
    /// freezes it with the SAME function `send_prepared_stop` uses, at this —
    /// the first shell-visible stop boundary — before the presentation change.
    /// The engine's own snapshot is never adopted: it was taken by a different
    /// process whose frontmost-app view may identify the app's own overlay
    /// rather than the external editor. Both stop triggers use shell capture.
    /// `_engine_stop_target` therefore remains advisory wire evidence only.
    pub fn accept_sidecar_transcribing_started(
        &mut self,
        session_id: String,
        _engine_stop_target: Option<crate::delivery::TargetSnapshot>,
        mut observer: impl FnMut(&AppState),
    ) -> Result<AppState, String> {
        if self.active_session.as_deref() != Some(session_id.as_str()) {
            return Ok(self.state.get());
        }
        if self.cancel_to_history_session.as_deref() != Some(session_id.as_str()) {
            // First-write-wins, so a stop the shell already drove keeps its own
            // capture; a hands-free stop captures here for the first time.
            self.capture_stop_destination();
            crate::telemetry::log_message(
                "stop_target_from_shell",
                &format!(
                    "session={session_id} captured={} pin={} engine_evidence={}",
                    self.stop_target.is_some(),
                    self.stop_focus_pin.is_some(),
                    _engine_stop_target.is_some()
                ),
            );
        }
        if matches!(self.state.get(), AppState::Recording { .. }) {
            self.apply(AppEvent::StopRecording, &mut observer)?;
        }
        Ok(self.state.get())
    }

    pub fn accept_sidecar_transcript(
        &mut self,
        session_id: String,
        mut transcript: String,
        send_enter: bool,
        raw_text: Option<String>,
        recording_ms: Option<u64>,
        engine_cancelled: bool,
        capture_warning: Option<String>,
        context_request_attempted: Option<bool>,
        context_original_binding: Option<bool>,
        mut on_inserted: impl FnMut(),
        observer: impl FnMut(&AppState),
    ) -> Result<AppState, String> {
        crate::telemetry::utterance_timeline::mark(|t| &mut t.accept_started_ms);
        if self.active_session.as_deref() != Some(session_id.as_str()) {
            crate::telemetry::log_message(
                "sidecar_transcript_dropped_stale",
                &format!("session={session_id} active={:?}", self.active_session),
            );
            return Ok(self.state.get());
        }

        // Detached start capture may finish after the stop boundary. Drain it
        // BEFORE policy selection below; the later drain in
        // `accept_sidecar_delivery` runs after the policy is already chosen.
        self.drain_start_capture();

        // zephyr SEND → paste THEN Enter. The Enter is fired inside the paste path
        // (deliver_submitting), timed off the paste-settle, so it never races the
        // async ⌘V and submits before the text lands.
        let mut want_enter = self.pending_send_enter || send_enter;
        let tail_is_reset = apply_sidecar_control_tail(&mut transcript, &mut want_enter)
            == SidecarControlTail::Reset;

        // Cancel (any source) finalize-to-history: a `prepare_cancel_to_history`
        // latch for this session, the engine's wire-level `cancelled`
        // diagnostics flag (a spoken "zephyr cancel" the KWS confirm spotter
        // recognized — the transcript TEXT may not contain any cancel tail,
        // e.g. when the trigger was mis-heard prose), or a textual cancel
        // tail as a fallback for older engine builds. All fold into the SAME
        // branch by design — each is a cancel, each stores the raw/L0
        // transcript to history, none delivers, none touches the clipboard.
        let is_cancel = engine_cancelled
            || tail_is_reset
            || self.cancel_to_history_session.as_deref() == Some(session_id.as_str());
        if is_cancel {
            // A cancel never sends Enter. `accept_cancel_to_history` below never
            // reads `want_enter` at all (it never delivers), so there is
            // nothing to assign here — this comment documents the invariant
            // that would otherwise need a dead `want_enter = false;`.
            // Prefer the raw/L0 (pre-AI-polish) transcript — the hard guarantee
            // that a cancelled record is never L1/L2/L3 polish, regardless of
            // what polish the engine ran for this stop. Falls back to
            // `transcript` only when the engine sent no raw text at all.
            let l0_text = raw_text
                .clone()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| transcript.clone());
            if l0_text.trim().is_empty() {
                // Nothing was captured — no history clutter, silent discard
                // (mirrors accept_sidecar_reset's cleanup exactly).
                self.cancel_to_history_session = None;
                if let Some(message) = capture_warning {
                    return self.accept_sidecar_error(session_id, message, observer);
                }
                return self.accept_sidecar_reset(session_id, observer);
            }
            return self.accept_cancel_to_history(
                session_id,
                l0_text,
                raw_text,
                recording_ms,
                capture_warning,
                observer,
            );
        }

        // Developer-mode streaming insertion already typed part of this text
        // into the destination; deliver only what is still missing. History
        // keeps the whole transcript.
        let streamed_full_transcript =
            crate::streaming_insertion::take_for_final(&session_id, &transcript)
                .map(|remainder| std::mem::replace(&mut transcript, remainder));
        if streamed_full_transcript.is_some() && transcript.trim().is_empty() && want_enter {
            if crate::streaming_insertion::press_enter() {
                on_inserted();
            }
            want_enter = false;
        }
        // A command-only Send has no payload. It must complete as an explicit
        // empty result, never submit whatever happens to be in focused app.
        if transcript.trim().is_empty() {
            want_enter = false;
        }
        let record = if transcript.trim().is_empty() {
            crate::host::host()
                .deliver_empty()
                .map_err(|err| err.to_string())?
        } else {
            let outcome = crate::host::host().deliver_final(crate::host::FinalDelivery {
                transcript: &transcript,
                want_enter,
                start_context_state: self.start_context.as_ref().map(|context| context.state),
                context_request_attempted,
                context_original_binding,
                start_target: self.start_target.as_ref(),
                stop_target: self.stop_target.as_ref(),
                start_pin: self.start_focus_pin.as_ref(),
                stop_pin: self.stop_focus_pin.as_ref(),
                start_content_fingerprint: self.start_content_fingerprint,
                on_inserted: &mut on_inserted,
            })?;
            match outcome {
                crate::host::FinalOutcome::Delivered(record) => record,
                crate::host::FinalOutcome::Recovery(record) => {
                    return self.accept_sidecar_delivery(
                        session_id,
                        record.with_raw(raw_text, recording_ms),
                        false,
                        observer,
                    );
                }
            }
        }
        .with_raw(raw_text, recording_ms);
        let mut record = record;
        if let Some(full) = streamed_full_transcript {
            record.transcript = full;
        }
        self.accept_sidecar_delivery(session_id, record, want_enter, observer)
    }

    /// Cancel (any source) finalize: build the `Cancelled` record and push it
    /// to encrypted history directly — deliberately NOT routed through
    /// `accept_sidecar_delivery`/`delivery_outcome_event_with_send` (no
    /// `execute_targeted_delivery`, no clipboard write, no paste, no Enter, no
    /// focus restore). Mirrors `accept_sidecar_reset`'s direct
    /// `self.state = ...; observer(...)` idiom for the same reason: this is a
    /// terminal transition outside the normal `AppEvent` pipeline.
    pub fn accept_cancel_to_history(
        &mut self,
        session_id: String,
        l0_text: String,
        raw_text: Option<String>,
        recording_ms: Option<u64>,
        capture_warning: Option<String>,
        mut observer: impl FnMut(&AppState),
    ) -> Result<AppState, String> {
        // Defensive re-check — mirrors every other accept_sidecar_* entry
        // point's stale-session guard, even though the caller already checked
        // this at function entry.
        if self.active_session.as_deref() != Some(session_id.as_str()) {
            crate::telemetry::log_message(
                "sidecar_cancel_to_history_dropped_stale",
                &format!("session={session_id} active={:?}", self.active_session),
            );
            return Ok(self.state.get());
        }

        crate::streaming_insertion::erase_on_cancel(&session_id);
        let target = crate::delivery::snapshot_target();
        let record = DeliveryRecord::new(
            rightkit_transcript_library::delivery::next_delivery_id(0),
            l0_text.clone(),
            rightkit_transcript_library::delivery::DeliveryOutcome::Cancelled,
            target,
        )
        .with_raw(raw_text, recording_ms);

        self.active_session = None;
        self.sync_sidecar_active_session();
        self.clear_start_capture();
        self.release_stop_boundary();
        self.cancel_to_history_session = None;

        self.last_delivery = Some(record.clone());
        // An explicit cancel is a terminal finalize too. If saving fails, still
        // leave a terminal state (below) and say so: the text stays in the
        // pill and in the surviving draft row, never reported as saved.
        let persist_error = self.persist_session_record(&session_id, record).err();
        if let Some(err) = persist_error.as_deref() {
            crate::telemetry::log_message(
                "history_finalize_failed",
                &format!("session={session_id} path=cancel error={err}"),
            );
        }
        self.draft_throttle = None;
        if let Some(record) = self.last_delivery.as_ref() {
            crate::telemetry::utterance_timeline::note_delivery(record);
            crate::telemetry::log_delivery(record, Some(&session_id), Some(false));
        }

        self.state.set(match (persist_error, capture_warning) {
            (Some(_), _) => AppState::Error {
                message: "Cancelled, but the transcript could not be saved to history.".to_string(),
                last_transcript: Some(l0_text),
            },
            (None, Some(message)) => AppState::Error {
                message,
                last_transcript: Some(l0_text),
            },
            (None, None) => AppState::Cancelled {
                transcript: l0_text,
            },
        });
        observer(&self.state.get());
        Ok(self.state.get())
    }

    pub fn accept_sidecar_reset(
        &mut self,
        session_id: String,
        mut observer: impl FnMut(&AppState),
    ) -> Result<AppState, String> {
        if self.active_session.as_deref() != Some(session_id.as_str()) {
            crate::telemetry::log_message(
                "sidecar_reset_dropped_stale",
                &format!("session={session_id} active={:?}", self.active_session),
            );
            return Ok(self.state.get());
        }
        self.active_session = None;
        self.sync_sidecar_active_session();
        self.clear_start_capture();
        self.release_stop_boundary();
        self.draft_throttle = None;
        self.state.set(AppState::Armed);
        observer(&self.state.get());
        Ok(self.state.get())
    }

    pub fn accept_sidecar_error(
        &mut self,
        session_id: String,
        message: String,
        mut observer: impl FnMut(&AppState),
    ) -> Result<AppState, String> {
        if self.active_session.as_deref() != Some(session_id.as_str()) {
            crate::telemetry::log_message(
                "sidecar_error_dropped_stale",
                &format!("session={session_id} active={:?}", self.active_session),
            );
            return Ok(self.state.get());
        }
        self.active_session = None;
        self.sync_sidecar_active_session();
        self.clear_start_capture();
        self.release_stop_boundary();
        // A cancel-to-history session whose finalize decode failed still ends
        // as Cancelled: the user asked to discard, so the engine's decode error
        // must not surface as Failed (the pill showed "failed" on a plain
        // fn+Escape — 2026-10-01 sessions 9/10). Mirrors `fail_prepared_stop`'s
        // cancel rollback exactly; the real message stays in telemetry and the
        // engine keeps the audio for its own retry.
        if self.cancel_to_history_session.as_deref() == Some(session_id.as_str()) {
            crate::telemetry::log_message(
                "cancel_to_history_finalize_error",
                &format!("session={session_id} error={message}"),
            );
            self.cancel_to_history_session = None;
            self.draft_throttle = None;
            self.pending_stop_context.clear();
            self.pending_send_enter = false;
            self.state.set(AppState::Cancelled {
                transcript: String::new(),
            });
            observer(&self.state.get());
            return Ok(self.state.get());
        }
        let _ = self.apply(AppEvent::Fail { message }, &mut observer);
        Ok(self.state.get())
    }

    /// Force the pill out of a stranded `Transcribing` state back to `Armed`. The
    /// in-flight session is abandoned (its terminal event never arrived); we do
    /// NOT deliver any transcript. Called by the consumer-loop watchdog.
    pub fn force_recover_to_armed(
        &mut self,
        mut observer: impl FnMut(&AppState),
    ) -> Option<PreparedCancel> {
        let command = self.prepare_cancel_backend(CancelSource::Watchdog);
        self.active_session = None;
        self.sync_sidecar_active_session();
        self.clear_start_capture();
        self.release_stop_boundary();
        self.state.set(AppState::Armed);
        observer(&self.state.get());
        command.map(|command| PreparedCancel::with_command(self.state.get(), command))
    }

    /// Re-arm after the sidecar acknowledged (or was restarted past) the
    /// watchdog's revocation of `session_id` (#149). No second cancel is sent.
    /// Returns false, changing nothing, when the session already moved on.
    pub fn recover_to_armed_after_revocation(
        &mut self,
        session_id: &str,
        mut observer: impl FnMut(&AppState),
    ) -> bool {
        if self.active_session.as_deref() != Some(session_id) {
            return false;
        }
        self.active_session = None;
        self.sync_sidecar_active_session();
        self.clear_start_capture();
        self.release_stop_boundary();
        self.state.set(AppState::Armed);
        observer(&self.state.get());
        true
    }

    pub fn repaste_last_delivery(&mut self) -> Result<DeliveryRecord, String> {
        self.ensure_engine_ready()?;
        match self.sidecar_repaste_last() {
            Ok(record) => {
                self.mirror_sidecar_manual_delivery(record.clone())?;
                Ok(record)
            }
            Err(err) if Self::is_no_last_delivery_error(&err) => {
                self.sync_sidecar_recent_history()?;
                let record = self.sidecar_repaste_last()?;
                self.mirror_sidecar_manual_delivery(record.clone())?;
                Ok(record)
            }
            Err(err) => Err(err),
        }
    }

    pub fn copy_last_delivery_to_clipboard(&mut self) -> Result<String, String> {
        self.ensure_engine_ready()?;
        match self.sidecar_copy_last() {
            Ok(transcript) => Ok(transcript),
            Err(err) if Self::is_no_last_delivery_error(&err) => {
                self.sync_sidecar_recent_history()?;
                self.sidecar_copy_last()
            }
            Err(err) => Err(err),
        }
    }

    pub fn apply(
        &mut self,
        event: AppEvent,
        observer: &mut impl FnMut(&AppState),
    ) -> Result<(), String> {
        let result = transition(self.state.get(), event)
            .map_err(|err| format!("invalid transition: {:?} + {:?}", err.state, err.event))?;
        self.state.set(result.next_state);
        observer(&self.state.get());
        Ok(())
    }

    pub fn ensure_engine_ready(&mut self) -> Result<(), String> {
        let fresh = self.sidecar.ensure_started()?;
        // Also cover an unexpected exit/restart that bypassed shutdown_backend:
        // a fresh process has no copy of the shell's cached config or vocabulary.
        if fresh {
            self.last_sent_engine_config = None;
            self.last_sent_vocab = None;
            self.sidecar.info_ready.store(false, Ordering::Release);
        }
        self.invalidate_sidecar_mirrors_if_generation_changed();
        self.sync_sidecar_runtime_snapshot()?;
        if !self.sidecar.info_ready.load(Ordering::Acquire) {
            self.sidecar_info_request()?;
            self.sidecar.info_ready.store(true, Ordering::Release);
        }
        Ok(())
    }

    /// Clone the sidecar supervisor handle so a caller can drive the
    /// (internally-synchronized) sidecar WITHOUT holding the global runtime
    /// lock. `prewarm` uses this to run the blocking `supervisor.start()`
    /// ready-wait lock-free — holding `runtime_lock` across that wait froze
    /// every pill/UI command behind it (the launch pill-hang, audit T-1).
    pub fn engine_supervisor_handle(
        &self,
    ) -> std::sync::Arc<crate::engine_supervisor::EngineSupervisor> {
        std::sync::Arc::clone(&self.sidecar.supervisor)
    }

    pub fn prepare_stop_backend_recording(
        &self,
        session_id: &str,
        send_enter: bool,
    ) -> PreparedEngineStop {
        let sidecar = &self.sidecar;
        // Legacy suffix kept alongside the typed `stop_kind` field during the
        // v2 transition (belt+braces) — see protocol v2 doc comment on
        // `StopKind` and dispatch #9/#10. Do not remove the suffix yet.
        let suffix = if send_enter { "send_enter" } else { "stop" };
        let request_id = format!("stop:{}:{suffix}", sidecar.supervisor.next_request_id());
        let stop_kind = if send_enter {
            StopKind::SendEnter
        } else {
            StopKind::Stop
        };
        let frame = engine_supervisor::build_request(
            &request_id,
            EngineSchemaName::TranscribingStarted,
            Some(session_id.to_string()),
            Some(EnginePayload::TranscribingStarted {
                stop_kind: Some(stop_kind),
                stop_target: None,
            }),
        );
        PreparedEngineStop {
            session_id: session_id.to_string(),
            supervisor: Arc::clone(&sidecar.supervisor),
            frame,
        }
    }

    /// Sibling of `prepare_stop_backend_recording` for the cancel-to-history
    /// finalize: same `PreparedEngineStop` shape (so `PreparedStop::send` /
    /// `send_prepared_stop`'s failure-rollback need no changes), but the frame
    /// carries `StopKind::CancelToHistory` instead of `Stop`/`SendEnter`.
    pub fn prepare_cancel_to_history_backend_recording(
        &self,
        session_id: &str,
    ) -> PreparedEngineStop {
        let sidecar = &self.sidecar;
        let request_id = format!(
            "stop:{}:cancel_to_history",
            sidecar.supervisor.next_request_id()
        );
        let frame = engine_supervisor::build_request(
            &request_id,
            EngineSchemaName::TranscribingStarted,
            Some(session_id.to_string()),
            Some(EnginePayload::TranscribingStarted {
                stop_kind: Some(StopKind::CancelToHistory),
                stop_target: None,
            }),
        );
        PreparedEngineStop {
            session_id: session_id.to_string(),
            supervisor: Arc::clone(&sidecar.supervisor),
            frame,
        }
    }

    pub fn prepare_cancel_backend(&self, source: CancelSource) -> Option<PreparedEngineCancel> {
        if let Some(session_id) = self.active_session.clone() {
            let sidecar = &self.sidecar;
            // Legacy suffix kept alongside the typed `stop_kind` field during
            // the v2 transition — see `StopKind` doc comment.
            let request_id = format!(
                "cancel:{}:cancel:{}",
                sidecar.supervisor.next_request_id(),
                source.as_str()
            );
            crate::telemetry::log_message(
                "cancel_lifecycle",
                &format!(
                    "phase=requested source={} session={} request_id={}",
                    source.as_str(),
                    session_id,
                    request_id
                ),
            );
            let frame = engine_supervisor::build_request(
                &request_id,
                EngineSchemaName::TranscribingStarted,
                Some(session_id.clone()),
                Some(EnginePayload::TranscribingStarted {
                    stop_kind: Some(StopKind::Cancel),
                    stop_target: None,
                }),
            );
            Some(PreparedEngineCancel {
                session_id,
                source,
                request_id,
                supervisor: Arc::clone(&sidecar.supervisor),
                active_session: Arc::clone(&sidecar.active_session),
                frame,
            })
        } else {
            crate::telemetry::log_message(
                "cancel_lifecycle",
                &format!("phase=ignored_no_active_session source={}", source.as_str()),
            );
            None
        }
    }

    pub fn sync_sidecar_active_session(&self) {
        if let Ok(mut active) = self.sidecar.active_session.lock() {
            *active = self.active_session.clone();
        }
    }

    pub fn sync_sidecar_runtime_snapshot(&mut self) -> Result<(), String> {
        self.invalidate_sidecar_mirrors_if_generation_changed();
        self.sync_sidecar_engine_config()?;
        self.sync_sidecar_vocabulary()
    }

    pub fn sync_sidecar_runtime_snapshot_if_running(&mut self) -> Result<(), String> {
        if !self.sidecar.supervisor.is_running() {
            return Ok(());
        }
        self.sync_sidecar_runtime_snapshot()
    }

    pub fn invalidate_sidecar_mirrors_if_generation_changed(&mut self) {
        let generation = self.sidecar.supervisor.generation();
        if generation != self.last_synced_sidecar_generation {
            self.last_sent_engine_config = None;
            self.last_sent_vocab = None;
            self.sidecar.info_ready.store(false, Ordering::Release);
            self.last_synced_sidecar_generation = generation;
        }
    }
}
