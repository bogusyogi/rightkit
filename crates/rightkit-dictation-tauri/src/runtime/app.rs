// Shared imports live on the module root (runtime.rs); `use super::*` keeps
// this file on the same namespace the include!-based layout provided.
#[allow(unused_imports)]
use super::*;

impl RuntimeApp {
    pub fn capture_stop_destination(&mut self) {
        let Self::Engine(runtime) = self;
        runtime.capture_stop_destination();
    }

    pub fn open_default() -> Result<Self, String> {
        // The resident engine sidecar owns model warmth, capture, ASR, and delivery runtime on
        // Windows and macOS. There is no other runtime and no selector for one.
        Self::open_sidecar()
    }

    pub fn app_state(&self) -> AppState {
        match self {
            Self::Engine(runtime) => runtime.state.get(),
        }
    }

    /// Whether `session_id` is the session the runtime currently owns. Side
    /// effects taken before an `accept_sidecar_*` call (which does its own
    /// stale-session check) must use this so a stale event cannot trigger
    /// them.
    pub fn is_active_session(&self, session_id: &str) -> bool {
        match self {
            Self::Engine(runtime) => runtime.active_session.as_deref() == Some(session_id),
        }
    }

    /// Default sidecar bootstrap. The shell routes the dictation worker
    /// lifecycle through the supervised the engine binary.
    #[allow(dead_code)]
    fn open_sidecar() -> Result<Self, String> {
        let (event_tx, event_rx) = mpsc::channel();
        let (supervisor, supervisor_rx) = EngineSupervisor::new(false);
        let supervisor = Arc::new(supervisor);
        let active_session = Arc::new(StdMutex::new(None::<String>));
        let engine_version = Arc::new(StdMutex::new(None::<String>));

        {
            let event_tx = event_tx.clone();
            let active_session = active_session.clone();
            let engine_version = engine_version.clone();
            std::thread::Builder::new()
                .name("hr-sidecar-bridge".to_string())
                .spawn(move || {
                    while let Ok(event) = supervisor_rx.recv() {
                        match event {
                            SupervisorEvent::Ready {
                                engine_version: version,
                            } => {
                                if let Ok(mut slot) = engine_version.lock() {
                                    *slot = Some(version);
                                }
                            }
                            SupervisorEvent::EngineEvent(engine_event) => {
                                let _ = event_tx.send(engine_event);
                            }
                            SupervisorEvent::Crashed { code } => {
                                crate::telemetry::log_message(
                                    "sidecar_crashed",
                                    &format!("code={code:?}"),
                                );
                                let session =
                                    active_session.lock().ok().and_then(|mut slot| slot.take());
                                if let Some(session_id) = session {
                                    let _ = event_tx.send(EngineEvent::EngineError {
                                        session_id,
                                        message: "engine crashed".to_string(),
                                    });
                                }
                            }
                            SupervisorEvent::SpawnFailed { message } => {
                                crate::telemetry::log_message("sidecar_spawn_failed", &message);
                                let session =
                                    active_session.lock().ok().and_then(|mut slot| slot.take());
                                if let Some(session_id) = session {
                                    let _ = event_tx.send(EngineEvent::EngineError {
                                        session_id,
                                        message,
                                    });
                                }
                            }
                        }
                    }
                })
                .map_err(|err| format!("spawn sidecar bridge: {err}"))?;
        }

        let state = SharedState::new(AppState::Armed);
        let sidecar = SidecarHandle {
            supervisor,
            active_session,
            engine_version,
            state: state.clone(),
            info_ready: Arc::new(AtomicBool::new(false)),
        };
        // Publish the internally-synchronized handle so the hot status reads
        // (`get_engine_info`, `get_app_state`, watchdog probes) and the long
        // prewarm waits can run WITHOUT the global runtime lock. Registration
        // is once-per-process, matching `APP`'s OnceLock.
        register_engine_sidecar(&sidecar);
        Ok(Self::Engine(EngineRuntime {
            state,
            sidecar,
            history: None,
            history_generation_seen: history_storage_generation(),
            recent_cache: Vec::new(),
            last_delivery: None,
            sequence: 0,
            active_session: None,
            stop_target: None,
            stop_focus_pin: None,
            start_capture_rx: None,
            start_dispatch_ready: None,
            start_target: None,
            start_focus_pin: None,
            start_context: None,
            start_content_fingerprint: None,
            pending_stop_context: PendingStopContextLatch::default(),
            pending_send_enter: false,
            event_rx: Some(event_rx),
            last_sent_engine_config: None,
            last_sent_vocab: None,
            last_synced_sidecar_generation: 0,
            cancel_to_history_session: None,
            draft_throttle: None,
        }))
    }

    pub fn engine_info(&mut self) -> Result<EngineInfoResult, String> {
        match self {
            Self::Engine(runtime) => runtime.engine_info(),
        }
    }

    /// Offline file transcription (free-tier feature). The shell sends only the
    /// selected file path; the sidecar owns decode + ASR.
    pub fn transcribe_file(
        &mut self,
        path: std::path::PathBuf,
    ) -> Result<rightkit_dictation_core::engine::FileTranscript, String> {
        match self {
            Self::Engine(runtime) => runtime.transcribe_file(path),
        }
    }

    pub fn recent_history(&mut self) -> Result<&[DeliveryRecord], String> {
        match self {
            Self::Engine(runtime) => runtime.recent_history(),
        }
    }

    pub fn prepare_start_dictation(
        &mut self,
        initial_target: crate::delivery::TargetSnapshot,
        observer: impl FnMut(&AppState),
    ) -> Result<PreparedStart, String> {
        match self {
            Self::Engine(runtime) => runtime.prepare_start(initial_target, observer),
        }
    }

    pub fn finalize_prepared_start(
        &mut self,
        result: PreparedStartResult,
        observer: impl FnMut(&AppState),
    ) -> Result<AppState, String> {
        match (self, result) {
            (_, PreparedStartResult::Immediate(state)) => Ok(state),
            (Self::Engine(runtime), PreparedStartResult::Engine { session_id }) => {
                let result = runtime.finalize_prepared_start(session_id.clone(), observer);
                if matches!(&result, Ok(AppState::Recording { .. })) {
                    crate::media_control::begin_for_session(&session_id);
                }
                result
            }
        }
    }

    pub fn fail_prepared_start(
        &mut self,
        session_id: &str,
        message: String,
        observer: impl FnMut(&AppState),
    ) -> AppState {
        crate::media_control::restore_for_session(session_id, "start_failed");
        match self {
            Self::Engine(runtime) => runtime.fail_prepared_start(session_id, message, observer),
        }
    }

    pub fn prepare_stop_dictation(
        &mut self,
        observer: impl FnMut(&AppState),
    ) -> Result<PreparedStop, String> {
        crate::media_control::restore_any("stop_requested");
        match self {
            Self::Engine(runtime) => {
                runtime.prepare_stop(false, StopOrigin::Generic, Vec::new(), observer)
            }
        }
    }

    pub fn prepare_stop_from_pill(
        &mut self,
        send_enter: bool,
        overlay_handles: Vec<isize>,
        observer: impl FnMut(&AppState),
    ) -> Result<PreparedStop, String> {
        crate::media_control::restore_any("stop_requested");
        match self {
            Self::Engine(runtime) => {
                runtime.prepare_stop(send_enter, StopOrigin::Pill, overlay_handles, observer)
            }
        }
    }

    /// Generic send action: stop + paste + Enter once paste lands.
    pub fn prepare_stop_and_send(
        &mut self,
        observer: impl FnMut(&AppState),
    ) -> Result<PreparedStop, String> {
        crate::media_control::restore_any("stop_requested");
        match self {
            Self::Engine(runtime) => {
                runtime.prepare_stop(true, StopOrigin::Generic, Vec::new(), observer)
            }
        }
    }

    /// Cancel (any source) finalize-to-history: the proven, non-blocking STOP
    /// path (`PreparedStop`/`prepare_stop`'s lock/generation/ordering), but the
    /// engine frame requests `StopKind::CancelToHistory` instead of
    /// `Stop`/`SendEnter` — so `accept_sidecar_transcript` stores the raw/L0
    /// transcript to encrypted history as `Cancelled` with no clipboard write,
    /// no paste, no Enter, no OS delivery. Session-keyed latch, so a stale
    /// value can never affect a later session. `send_enter` is always false.
    pub fn prepare_cancel_to_history(
        &mut self,
        observer: impl FnMut(&AppState),
    ) -> Result<PreparedStop, String> {
        crate::media_control::restore_any("cancel_to_history_requested");
        match self {
            Self::Engine(runtime) => runtime.prepare_cancel_to_history(observer),
        }
    }

    pub fn fail_prepared_stop(
        &mut self,
        session_id: &str,
        message: String,
        observer: impl FnMut(&AppState),
    ) -> AppState {
        crate::media_control::restore_for_session(session_id, "stop_failed");
        match self {
            Self::Engine(runtime) => runtime.fail_prepared_stop(session_id, message, observer),
        }
    }

    pub fn accept_sidecar_delivery(
        &mut self,
        session_id: String,
        record: DeliveryRecord,
        send_enter: bool,
        observer: impl FnMut(&AppState),
    ) -> Result<AppState, String> {
        crate::media_control::restore_for_session(&session_id, "delivery_completed");
        match self {
            Self::Engine(runtime) => {
                runtime.accept_sidecar_delivery(session_id, record, send_enter, observer)
            }
        }
    }

    pub fn accept_sidecar_transcript(
        &mut self,
        session_id: String,
        transcript: String,
        send_enter: bool,
        raw_text: Option<String>,
        recording_ms: Option<u64>,
        cancelled: bool,
        capture_warning: Option<String>,
        context_request_attempted: Option<bool>,
        context_original_binding: Option<bool>,
        on_inserted: impl FnMut(),
        observer: impl FnMut(&AppState),
    ) -> Result<AppState, String> {
        crate::media_control::restore_for_session(&session_id, "transcript_final");
        match self {
            Self::Engine(runtime) => runtime.accept_sidecar_transcript(
                session_id,
                transcript,
                send_enter,
                raw_text,
                recording_ms,
                cancelled,
                capture_warning,
                context_request_attempted,
                context_original_binding,
                on_inserted,
                observer,
            ),
        }
    }

    /// Crash-recovery draft history rows: throttled create-or-replace of the
    /// in-progress draft row for `session_id` from the engine's committed
    /// (L0/raw) `TranscriptPartial` text.
    pub fn upsert_transcript_draft(
        &mut self,
        session_id: String,
        text: String,
        revision: u64,
    ) -> Result<(), String> {
        match self {
            Self::Engine(runtime) => runtime.upsert_transcript_draft(session_id, text, revision),
        }
    }

    pub fn accept_sidecar_transcribing_started(
        &mut self,
        session_id: String,
        stop_target: Option<crate::delivery::TargetSnapshot>,
        observer: impl FnMut(&AppState),
    ) -> Result<AppState, String> {
        crate::media_control::restore_for_session(&session_id, "transcribing_started");
        match self {
            Self::Engine(runtime) => {
                runtime.accept_sidecar_transcribing_started(session_id, stop_target, observer)
            }
        }
    }

    pub fn accept_sidecar_reset(
        &mut self,
        session_id: String,
        observer: impl FnMut(&AppState),
    ) -> Result<AppState, String> {
        crate::media_control::restore_for_session(&session_id, "session_reset");
        match self {
            Self::Engine(runtime) => runtime.accept_sidecar_reset(session_id, observer),
        }
    }

    pub fn accept_sidecar_error(
        &mut self,
        session_id: String,
        message: String,
        observer: impl FnMut(&AppState),
    ) -> Result<AppState, String> {
        crate::media_control::restore_for_session(&session_id, "sidecar_error");
        match self {
            Self::Engine(runtime) => runtime.accept_sidecar_error(session_id, message, observer),
        }
    }

    /// Watchdog recovery: force a pill stranded in `Transcribing` back to `Armed`
    /// when no terminal event ever arrived (supersession race, dropped event, or a
    /// sidecar crash mid-decode). Drops the in-flight session; the stale transcript
    /// is not delivered.
    pub fn force_recover_to_armed(
        &mut self,
        observer: impl FnMut(&AppState),
    ) -> Option<PreparedCancel> {
        crate::media_control::restore_any("watchdog_recovery");
        match self {
            Self::Engine(runtime) => runtime.force_recover_to_armed(observer),
        }
    }

    /// Watchdog recovery after the sidecar acknowledged revocation (#149).
    pub fn recover_to_armed_after_revocation(
        &mut self,
        session_id: &str,
        observer: impl FnMut(&AppState),
    ) -> bool {
        match self {
            Self::Engine(runtime) => {
                let recovered = runtime.recover_to_armed_after_revocation(session_id, observer);
                if recovered {
                    crate::media_control::restore_any("watchdog_recovery");
                }
                recovered
            }
        }
    }

    pub fn sidecar_is_transcribing_session(&self, session_id: &str) -> Result<bool, String> {
        match self {
            Self::Engine(runtime) => runtime.sidecar_is_transcribing_session(session_id),
        }
    }

    pub fn prepare_cancel(&mut self, source: CancelSource) -> PreparedCancel {
        crate::media_control::restore_any("cancel_requested");
        match self {
            Self::Engine(runtime) => {
                let command = runtime.prepare_cancel_backend(source);
                runtime.stop_target = None;
                runtime.stop_focus_pin = None;
                crate::host::host().release_pending_accessibility_observation();
                runtime.pending_stop_context.clear();
                runtime.state.set(AppState::Armed);
                runtime.active_session = None;
                runtime.pending_send_enter = false;
                let state = runtime.state.get();
                match command {
                    Some(command) => PreparedCancel::with_command(state, command),
                    None => PreparedCancel::without_command(state),
                }
            }
        }
    }

    /// Expire an already-terminal error without granting a stale pill timer
    /// authority to cancel a later recording. Unlike `cancel`, this is a strict
    /// state-conditional reset and never sends a sidecar command.
    pub fn recover_terminal_error(
        &mut self,
        expected_generation: Option<u64>,
    ) -> Result<AppState, String> {
        match self {
            Self::Engine(runtime) => {
                if !runtime
                    .state
                    .terminal_error_recovery_permits(expected_generation)
                {
                    return Err(
                        "terminal error recovery rejected: stale or no longer in Error".to_string(),
                    );
                }
                if runtime.active_session.is_some() {
                    return Err(
                        "terminal error recovery rejected: active interaction exists".to_string(),
                    );
                }
                runtime.stop_target = None;
                runtime.stop_focus_pin = None;
                crate::host::host().release_pending_accessibility_observation();
                runtime.pending_stop_context.clear();
                runtime.pending_send_enter = false;
                runtime.state.set(AppState::Armed);
                Ok(runtime.state.get())
            }
        }
    }

    pub fn repaste_last_delivery(&mut self) -> Result<DeliveryRecord, String> {
        match self {
            Self::Engine(runtime) => runtime.repaste_last_delivery(),
        }
    }

    /// The real history id of the most recent delivery — used to resolve the pill
    /// clipboard's "current" sentinel to the actual row, so an edit made in the
    /// fallback popup persists to History, not just the in-memory display override.
    pub fn last_delivery_id(&mut self) -> Option<String> {
        match self {
            Self::Engine(runtime) => runtime
                .last_delivery
                .as_ref()
                .map(|record| record.delivery_id.clone()),
        }
    }

    pub fn copy_last_delivery_to_clipboard(&mut self) -> Result<String, String> {
        match self {
            Self::Engine(runtime) => runtime.copy_last_delivery_to_clipboard(),
        }
    }

    pub fn dev_seed_history_record(
        &mut self,
        transcript: String,
    ) -> Result<DeliveryRecord, String> {
        let text = transcript.trim();
        if text.is_empty() {
            return Err("seed transcript is empty".to_string());
        }
        let record = DeliveryRecord::new(
            rightkit_transcript_library::delivery::next_delivery_id(0),
            text,
            crate::delivery::DeliveryOutcome::CopiedFallback {
                reason: CopyFallbackReason::NoTextField,
            },
            crate::delivery::TargetSnapshot {
                process_id: None,
                process_name: Some("native-regression-gate".to_string()),
                window_title: Some("Native Regression Gate".to_string()),
                window_handle: None,
                focused_control_handle: None,
                foreground_target: None,
                focused_text_input: Some(false),
                is_elevated: Some(false),
                stop_continuity: None,
                exe_path: None,
            },
        );
        match self {
            Self::Engine(runtime) => {
                runtime.dev_seed_history_record(record.clone())?;
                Ok(record)
            }
        }
    }

    pub fn delete_history_item(&mut self, delivery_id: &str) -> Result<bool, String> {
        match self {
            Self::Engine(runtime) => runtime.delete_history_item(delivery_id),
        }
    }

    pub fn rotate_history_key(&mut self) -> Result<(), String> {
        match self {
            Self::Engine(runtime) => runtime.rotate_history_key(),
        }
    }

    pub fn query_history(&mut self, query: &HistoryQuery) -> Result<HistoryPage, String> {
        match self {
            Self::Engine(runtime) => runtime.query_history(query),
        }
    }

    pub fn delete_history_matching(&mut self, filter: &HistoryFilter) -> Result<usize, String> {
        match self {
            Self::Engine(runtime) => runtime.delete_history_matching(filter),
        }
    }

    pub fn prune_history_before(&mut self, cutoff_ms: u64) -> Result<usize, String> {
        match self {
            Self::Engine(runtime) => runtime.prune_history_before(cutoff_ms),
        }
    }

    pub fn update_history_item(
        &mut self,
        delivery_id: &str,
        transcript: String,
    ) -> Result<bool, String> {
        match self {
            Self::Engine(runtime) => runtime.update_history_item(delivery_id, transcript),
        }
    }

    pub fn clear_history(&mut self) -> Result<(), String> {
        match self {
            Self::Engine(runtime) => runtime.clear_history(),
        }
    }

    /// Phase A2: start the equinox wake-word listener.The
    /// sidecar returns an availability error until a packaged wake scorer is
    /// available.
    pub fn start_wake_listen(&mut self, params: WakeListenParams) -> Result<(), String> {
        match self {
            Self::Engine(runtime) => runtime.start_wake_listen(params),
        }
    }

    /// Phase A2: stop the equinox wake-word listener. Idempotent.
    pub fn stop_wake_listen(&mut self) -> Result<(), String> {
        match self {
            Self::Engine(runtime) => runtime.stop_wake_listen(),
        }
    }

    pub fn prewarm(&mut self) -> Result<(), String> {
        match self {
            Self::Engine(runtime) => runtime.prewarm(),
        }
    }

    /// Sidecar supervisor handle for lock-free prewarm (audit T-1). Always
    /// `Some`: the one runtime is the engine sidecar.
    pub fn engine_supervisor_handle(
        &self,
    ) -> Option<std::sync::Arc<crate::engine_supervisor::EngineSupervisor>> {
        match self {
            Self::Engine(runtime) => Some(runtime.engine_supervisor_handle()),
        }
    }

    pub fn sync_sidecar_runtime_snapshot_if_running(&mut self) -> Result<(), String> {
        match self {
            Self::Engine(runtime) => runtime.sync_sidecar_runtime_snapshot_if_running(),
        }
    }

    pub fn shutdown_resident_backend(&mut self) {
        let Self::Engine(runtime) = self;
        runtime.shutdown_backend();
    }

    /// Take the async engine-event receiver. Returns `None` if already taken. Call once at
    /// startup and spawn a consumer thread.
    ///
    /// The matching sender lives inside the sidecar bridge and is reused while
    /// the resident app process stays alive.
    pub fn take_engine_event_receiver(&mut self) -> Option<Receiver<EngineEvent>> {
        match self {
            Self::Engine(runtime) => runtime.event_rx.take(),
        }
    }
}

// The single production engine runtime. It owns the shell-local state machine,
// history mirror, and UI-facing orchestration around the supervised
// the engine sidecar process.
