#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{delivery_history::*, engine::*, engine_support::*, finalize_transcript::*};

/// Whether a history row can be the engine's last delivery (see `replace_recent_history`).
fn is_last_delivery_candidate(record: &DeliveryRecord) -> bool {
    use rightkit_transcript_library::delivery::{DeliveryOutcome, DeliverySource};
    matches!(record.source, None | Some(DeliverySource::Dictation))
        && !matches!(
            record.outcome,
            DeliveryOutcome::Draft | DeliveryOutcome::Cancelled
        )
}

impl EngineRuntime {
    pub fn new(models_base: PathBuf) -> Self {
        Self {
            state: EngineState::Idle,
            last_delivery: None,
            recent: VecDeque::with_capacity(RECENT_HISTORY_CAP),
            focus: Arc::new(Mutex::new(FocusTracker::new())),
            sequence: 0,
            terminal_tombstones: VecDeque::with_capacity(TERMINAL_TOMBSTONE_CAP),
            models_base,
            worker: None,
            pending_send_enter: false,
            pending_local_only: false,
            stop_started_at: None,
        }
    }

    pub fn state(&self) -> &EngineState {
        &self.state
    }

    pub fn engine_info(&self) -> EngineInfo {
        EngineInfo {
            mode: "sidecar".to_string(),
            cold_load_s: None,
            engine_version: Some(format!("rightkit-engine/{}", env!("CARGO_PKG_VERSION"))),
        }
    }

    pub fn recent_history(&self, limit: usize) -> Vec<DeliveryRecord> {
        self.recent.iter().take(limit).cloned().collect()
    }

    pub fn replace_recent_history(&mut self, records: Vec<DeliveryRecord>) {
        self.recent.clear();
        for record in records.into_iter().take(RECENT_HISTORY_CAP) {
            self.recent.push_back(record);
        }
        // Only the user's own finished dictations can be re-pasted, copied or acted on by a voice
        // command: never a cancelled or draft row, a file transcription or a synced row.
        self.last_delivery = self
            .recent
            .iter()
            .find(|record| is_last_delivery_candidate(record))
            .cloned();
    }

    /// Record the shell's last real paste as the engine's last delivery, without adding it to the
    /// recent list (the shell owns history). Voice undo and the casing commands read it.
    pub fn set_last_delivery(&mut self, record: DeliveryRecord) {
        self.last_delivery = Some(record);
    }

    /// Shared focus tracker so the worker thread can refresh the live current
    /// focus during recording while the runtime owns the final delivery decision.
    pub fn focus(&self) -> Arc<Mutex<FocusTracker>> {
        self.focus.clone()
    }

    /// Ensure the ASR worker is spawned. IPC startup calls this before emitting
    /// `EngineInfo`, so a ready sidecar means the resident model worker is hot.
    pub fn ensure_worker(&mut self) -> Result<(), String> {
        if self.worker.is_some() {
            return Ok(());
        }
        let ep = AsrEp::resolve_default();
        let handle = spawn_worker(self.models_base.clone(), ep, self.focus.clone())?;
        self.worker = Some(handle);
        Ok(())
    }

    /// Brief-lock accessor: ensure the worker thread is spawned, then hand back a
    /// CLONED `WorkerHandle`. Callers do the slow worker round-trip (model load /
    /// file transcribe) on this clone WITHOUT holding the `EngineRuntime` mutex —
    /// this is what stops one in-flight op from blocking every other request.
    pub fn worker_handle(&mut self) -> Result<WorkerHandle, String> {
        self.ensure_worker()?;
        self.worker
            .as_ref()
            .cloned()
            .ok_or_else(|| "worker not available".to_string())
    }

    /// True when no recording/transcribe session is active (safe to swap models).
    pub fn is_idle(&self) -> bool {
        !matches!(
            self.state,
            EngineState::Recording { .. } | EngineState::Transcribing { .. }
        )
    }

    pub fn worker_event_rx(&self) -> Option<Arc<Mutex<Receiver<WorkerEvent>>>> {
        self.worker.as_ref().map(|w| w.event_rx.clone())
    }

    pub fn take_worker_event(&self) -> Option<WorkerEvent> {
        let rx = self.worker.as_ref()?.event_rx.clone();
        let event = rx.lock().try_recv().ok();
        event
    }

    pub fn transcribe_file_path(&mut self, path: PathBuf) -> Result<FileTranscript, String> {
        self.ensure_worker()?;
        let worker = self
            .worker
            .as_ref()
            .ok_or_else(|| "worker not available".to_string())?;
        let (reply_tx, reply_rx) = channel();
        worker.send(WorkerCmd::TranscribeFile {
            path,
            reply: reply_tx,
        })?;
        reply_rx
            .recv_timeout(std::time::Duration::from_secs(600))
            .map_err(|error| match error {
                std::sync::mpsc::RecvTimeoutError::Timeout => {
                    "stdio_transcription_reply_timeout: no worker reply in 600 s".to_string()
                }
                std::sync::mpsc::RecvTimeoutError::Disconnected => {
                    "worker transcription reply dropped".to_string()
                }
            })?
    }

    /// Ensure the selected backend/language is loaded and warmed before capture
    /// is allowed to start.
    pub fn ensure_current_model_ready(&mut self) -> Result<(), String> {
        self.ensure_worker()?;
        let worker = self
            .worker
            .as_ref()
            .ok_or_else(|| "worker not available".to_string())?;
        worker.reload_model().map(|_| ())
    }

    /// Prepare the selected model after settings changes, but never swap ASR
    /// under an active recording/transcribe session.
    pub fn prepare_model_if_idle(&mut self) -> Result<(), String> {
        if matches!(
            self.state,
            EngineState::Recording { .. } | EngineState::Transcribing { .. }
        ) {
            return Ok(());
        }
        self.ensure_current_model_ready()
    }

    /// Begin recording for a session. Idempotent — a re-entry is a no-op.
    pub fn start_dictation(&mut self, session_id: &str) -> Result<(), String> {
        if matches!(&self.state, EngineState::Recording { session_id: s } if s == session_id) {
            return Ok(());
        }
        self.ensure_current_model_ready()?;
        self.start_dictation_warmed(session_id)
    }

    /// Begin recording — the FAST PATH used by the IPC `StartDictation`
    /// handler: flips state under this (brief) lock and hands back the
    /// shared focus tracker so the caller can run the UIA/AX-bound focus
    /// snapshot and dispatch the worker's `StartRecording` command OUTSIDE
    /// the runtime mutex. A hung target process's accessibility server must
    /// never wedge Stop/Cancel/GetState behind this lock.
    ///
    /// Rejects instead of silently overwriting when a previous session is
    /// still `Recording` under a DIFFERENT session id or `Transcribing` (a
    /// finalize is in flight) — overwriting `self.state` would force-discard
    /// whatever was mid-finalize, and its eventual `TranscriptFinal` would
    /// then land against the stale-guard as a spurious no-op. Re-entry with
    /// the SAME session id while already `Recording` is idempotent: returns
    /// `Ok(None)` and the caller must skip re-snapshotting focus / re-
    /// sending `StartRecording`.
    pub fn begin_recording(
        &mut self,
        session_id: &str,
        recording_target: Option<rightkit_transcript_library::delivery::TargetSnapshot>,
        recording_context: Option<rightkit_dictation_core::engine::RecordingContext>,
    ) -> Result<Option<Arc<Mutex<FocusTracker>>>, String> {
        match &self.state {
            EngineState::Recording { session_id: s } if s == session_id => {
                // Duplicate RecordingStarted is idempotent. The detached
                // RecordingContextCaptured frame owns later enrichment; a
                // repeated start carrying None must never erase it.
                Ok(None)
            }
            EngineState::Recording { .. } | EngineState::Transcribing { .. } => {
                Err("busy: finalizing previous dictation".to_string())
            }
            EngineState::Idle => {
                self.focus
                    .lock()
                    .set_recording_start_target(recording_target);
                self.focus
                    .lock()
                    .set_recording_start_context(recording_context);
                self.state = EngineState::Recording {
                    session_id: session_id.to_string(),
                };
                self.sequence = self.sequence.saturating_add(1);
                Ok(Some(self.focus.clone()))
            }
            EngineState::Error {
                session_id: terminal,
                ..
            } => {
                let terminal = terminal.clone();
                self.record_terminal_tombstone(&terminal);
                self.focus
                    .lock()
                    .set_recording_start_target(recording_target);
                self.focus
                    .lock()
                    .set_recording_start_context(recording_context);
                self.state = EngineState::Recording {
                    session_id: session_id.to_string(),
                };
                self.sequence = self.sequence.saturating_add(1);
                Ok(Some(self.focus.clone()))
            }
        }
    }

    pub(crate) fn interaction_is_live(&self, session_id: &str) -> bool {
        !self.terminal_tombstones.iter().any(|id| id == session_id)
            && matches!(
                &self.state,
                EngineState::Recording { session_id: active }
                    | EngineState::Transcribing { session_id: active }
                    if active == session_id
            )
    }

    pub(crate) fn set_recording_context(
        &mut self,
        session_id: &str,
        recording_target: Option<rightkit_transcript_library::delivery::TargetSnapshot>,
        context: rightkit_dictation_core::engine::RecordingContext,
    ) -> bool {
        // Start capture is detached and can land just after a short
        // recording's stop. Finalize snapshots the context after final ASR, so
        // the same live session may still accept it while Transcribing.
        if !self.interaction_is_live(session_id) {
            return false;
        }
        self.focus
            .lock()
            .set_recording_start_target(recording_target);
        self.focus.lock().set_recording_start_context(Some(context));
        true
    }

    pub(crate) fn record_terminal_tombstone(&mut self, session_id: &str) {
        if self.terminal_tombstones.iter().any(|id| id == session_id) {
            return;
        }
        if self.terminal_tombstones.len() == TERMINAL_TOMBSTONE_CAP {
            self.terminal_tombstones.pop_front();
        }
        self.terminal_tombstones.push_back(session_id.to_string());
    }

    /// Cheap peek used by the finalize generation check (finding F3): true
    /// when `generation` still matches the live counter. The counter is
    /// bumped by `begin_recording` (a new session started) and `cancel` (the
    /// active session was aborted) — either invalidates any finalize
    /// snapshot taken before it, so a stale finalize's commit phase knows to
    /// discard instead of clobbering whatever runs after.
    pub(crate) fn sequence_matches(&self, generation: u64) -> bool {
        self.sequence == generation
    }

    /// Begin recording assuming the model is already warm — full synchronous
    /// convenience kept for tests and any other direct (non-IPC, non-`Arc<
    /// Mutex<..>>`) caller. Delegates the state transition + reject policy
    /// to `begin_recording`, then runs the focus snapshot and worker
    /// dispatch inline: safe here because a direct `&mut EngineRuntime`
    /// caller has no concurrent lock holder that could be blocked by it.
    pub fn start_dictation_warmed(&mut self, session_id: &str) -> Result<(), String> {
        match self.begin_recording(session_id, None, None)? {
            Some(_) => {}
            None => return Ok(()),
        }
        // Target capture is deliberately deferred to the stop boundary. The
        // recording start path must not probe AX/UIA or harvest screen context.
        if let Some(w) = &self.worker {
            // The dummy worker used in tests swallows the command. The real
            // worker only fails to receive if the sidecar is being torn down,
            // in which case the state machine below is already invalid.
            let _ = w.send(WorkerCmd::StartRecording {
                session_id: session_id.to_string(),
            });
        }
        Ok(())
    }

    /// Begin the transcribe. Async: the actual decode happens off-thread and
    /// `finalize_transcript` is called with the result. Returns `Err` only on
    /// illegal transitions; missing-model failures are reported via the
    /// `WorkerEvent::StartFailed` channel.
    /// Destination the worker froze at the stop moment (first-write-wins),
    /// carried to the shell so a hands-free stop is delivered where the
    /// user was when they said it, not where focus is when the IPC lands.
    pub fn captured_stop_target(
        &self,
    ) -> Option<rightkit_transcript_library::delivery::TargetSnapshot> {
        self.focus.lock().captured_target()
    }

    pub fn begin_stop(
        &mut self,
        session_id: &str,
        send_enter: bool,
        local_only: bool,
    ) -> Result<(), String> {
        match &self.state {
            EngineState::Recording { session_id: s } if s == session_id => {
                self.state = EngineState::Transcribing {
                    session_id: session_id.to_string(),
                };
                self.pending_send_enter = send_enter;
                self.pending_local_only = local_only;
                let stop_origin = Instant::now();
                self.stop_started_at = Some(stop_origin);
                // Timeline telemetry origin: fixed before the worker can see
                // the stop so every later offset is measured from here.
                rightkit_dictation_inference::timeline_recorder::begin_stop(
                    session_id,
                    stop_origin,
                );
                if let Some(w) = &self.worker {
                    let _ = w.send(WorkerCmd::StopRecording { send_enter });
                }
                Ok(())
            }
            // Already transcribing / idle — idempotent no-op (hotkey+click race).
            _ => Ok(()),
        }
    }

    /// Abort the active session. Idempotent.
    pub fn cancel(&mut self, session_id: &str) {
        self.cancel_traced(session_id, "internal", "internal");
    }

    pub fn cancel_traced(&mut self, session_id: &str, request_id: &str, source: &str) {
        match &self.state {
            EngineState::Recording { session_id: s }
            | EngineState::Transcribing { session_id: s }
                if s == session_id =>
            {
                self.record_terminal_tombstone(session_id);
                self.state = EngineState::Idle;
                self.pending_send_enter = false;
                self.pending_local_only = false;
                self.stop_started_at = None;
                // Bump the generation (finding F3): a finalize snapshot taken
                // before this Cancel must discard its result at commit time
                // instead of clobbering whatever runs after this Idle.
                self.sequence = self.sequence.saturating_add(1);
                self.focus.lock().reset();
                if let Some(w) = &self.worker {
                    let dispatched = w.send(WorkerCmd::Cancel).is_ok();
                    tracing::info!(
                        target: "cancel_lifecycle",
                        phase = "worker_dispatched",
                        source,
                        session_id,
                        request_id,
                        dispatched,
                        "external cancel worker dispatch"
                    );
                }
            }
            _ => {}
        }
    }

    /// Clear only the matching recording after capture fails to become live.
    /// The worker has already stopped its local session; this closes engine
    /// authority before IPC emits a session-bound error to the shell.
    pub fn fail_recording_capture(&mut self, session_id: &str) -> bool {
        if !matches!(
            &self.state,
            EngineState::Recording { session_id: active } if active == session_id
        ) {
            return false;
        }
        self.record_terminal_tombstone(session_id);
        self.state = EngineState::Idle;
        self.pending_send_enter = false;
        self.pending_local_only = false;
        self.stop_started_at = None;
        self.sequence = self.sequence.saturating_add(1);
        self.focus.lock().reset();
        true
    }

    /// Apply a finalized transcript and return the sidecar-owned outcome for
    /// the shell to mirror into its UI state/history.
    pub fn finalize_transcript(
        &mut self,
        session_id: &str,
        result: Result<String, String>,
    ) -> Result<FinalizeOutcome, String> {
        self.finalize_transcript_with_audio_secs(session_id, result, None)
    }
}
