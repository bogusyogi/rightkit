// Shared imports live on the module root (runtime.rs); `use super::*` keeps
// this file on the same namespace the include!-based layout provided.
#[allow(unused_imports)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelSource {
    WebviewCommand,
    FnEscape,
    PillCancelButton,
    PttChordAbort,
    KeyboardCancelHook,
    ShortcutIntent,
    Watchdog,
}

impl CancelSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WebviewCommand => "webview_command",
            Self::FnEscape => "fn_escape",
            Self::PillCancelButton => "pill_cancel_button",
            Self::PttChordAbort => "ptt_chord_abort",
            Self::KeyboardCancelHook => "keyboard_cancel_hook",
            Self::ShortcutIntent => "shortcut_intent",
            Self::Watchdog => "watchdog",
        }
    }
}

/// The longest a stop's finalize waits for the provider-key delivery of its generation: the
/// delivery's own ack bound (3 s) plus margin. On expiry the stop goes on and the finalize uses
/// local polish unless the keys land first (logged by the supervisor).
const PROVIDER_KEYS_FINALIZE_WAIT: Duration = Duration::from_secs(4);

pub enum RuntimeApp {
    Engine(EngineRuntime),
}

pub struct PreparedStop {
    state: AppState,
    command: Option<PreparedEngineStop>,
    capture_destination: bool,
}

pub struct PreparedEngineStop {
    pub(crate) session_id: String,
    pub(crate) supervisor: Arc<EngineSupervisor>,
    pub(crate) frame: EngineFrame,
}

pub struct PreparedStopError {
    pub session_id: String,
    pub message: String,
}

pub struct PreparedCancel {
    state: AppState,
    command: Option<PreparedEngineCancel>,
}

pub struct PreparedStart {
    state: AppState,
    command: Option<PreparedEngineStart>,
}

pub struct PreparedEngineStart {
    pub(crate) session_id: String,
    pub(crate) supervisor: Arc<EngineSupervisor>,
    pub(crate) frame: EngineFrame,
    /// Config and vocabulary pushes, in order, for an engine this start spawns or finds respawned.
    pub(crate) snapshot_frames: Vec<EngineFrame>,
    /// The engine generation `snapshot_frames` were prepared against.
    pub(crate) generation_seen: u64,
    pub(crate) streaming_generation: Arc<StdMutex<Option<u64>>>,
}

pub struct PreparedEngineCancel {
    pub(crate) session_id: String,
    pub(crate) source: CancelSource,
    pub(crate) request_id: String,
    pub(crate) supervisor: Arc<EngineSupervisor>,
    pub(crate) active_session: Arc<StdMutex<Option<String>>>,
    pub(crate) frame: EngineFrame,
}

impl PreparedStop {
    pub fn without_command(state: AppState) -> Self {
        Self {
            state,
            command: None,
            capture_destination: false,
        }
    }

    pub fn with_command(
        state: AppState,
        command: PreparedEngineStop,
        capture_destination: bool,
    ) -> Self {
        Self {
            state,
            command: Some(command),
            capture_destination,
        }
    }

    pub fn captures_destination(&self) -> bool {
        self.capture_destination
    }

    pub fn send(self) -> Result<AppState, PreparedStopError> {
        let Some(command) = self.command else {
            return Ok(self.state);
        };
        crate::telemetry::utterance_timeline::bind_session(&command.session_id);
        crate::telemetry::utterance_timeline::mark(|t| &mut t.stop_sent_ms);
        // The audio was sealed at the release before this stop was built (`seal_capture_for_stop`),
        // so waiting here for a provider-key delivery in flight (bounded; the start registered its
        // claim before it returned) never lengthens the recording.
        let sent = command
            .supervisor
            .send_stop(&command.frame, PROVIDER_KEYS_FINALIZE_WAIT);
        // `send` blocks on the engine's accepted reply, so this is the accept.
        crate::telemetry::utterance_timeline::mark(|t| &mut t.stop_accepted_ms);
        sent.map(|()| {
            // #159: bound the final-transcript wait by audio length.
            command.supervisor.arm_final_wait(&command.session_id);
            self.state
        })
        .map_err(|message| PreparedStopError {
            session_id: command.session_id,
            message,
        })
    }
}

impl PreparedCancel {
    pub fn without_command(state: AppState) -> Self {
        Self {
            state,
            command: None,
        }
    }

    pub fn with_command(state: AppState, command: PreparedEngineCancel) -> Self {
        Self {
            state,
            command: Some(command),
        }
    }

    pub fn send(self) -> AppState {
        let Some(command) = self.command else {
            return self.state;
        };

        if let Err(err) = command.supervisor.send(&command.frame) {
            crate::telemetry::log_message("sidecar_cancel", &err);
            crate::telemetry::log_message(
                "cancel_lifecycle",
                &format!(
                    "phase=dispatch_failed source={} session={} request_id={}",
                    command.source.as_str(),
                    command.session_id,
                    command.request_id
                ),
            );
            command.supervisor.shutdown();
        } else {
            crate::telemetry::log_message(
                "cancel_lifecycle",
                &format!(
                    "phase=dispatched source={} session={} request_id={}",
                    command.source.as_str(),
                    command.session_id,
                    command.request_id
                ),
            );
        }
        if let Ok(mut active) = command.active_session.lock() {
            *active = None;
        }
        self.state
    }
}

impl PreparedEngineCancel {
    /// The session-correlated cancel of a start whose RPC ended in an error or timeout after its
    /// frame may have reached the engine (`ProductionStart::backend_start`). It carries the
    /// session id and the press's attempt id, so the engine cancels the session if the start did
    /// run and fences the session and attempt if it did not (a start that arrives later does not
    /// record). Goes through `send_session_cancel` (no key-gate wait, never spawns an engine). If
    /// the engine is running and does not accept the cancel, the engine is shut down (it cannot be
    /// trusted not to be recording); the next start respawns it.
    pub fn send_possibly_started(self) {
        let outcome = self.supervisor.send_session_cancel(&self.frame);
        let phase = match &outcome {
            Ok(()) => "ambiguous_start_dispatched",
            Err(_) => "ambiguous_start_dispatch_failed",
        };
        crate::telemetry::log_message(
            "cancel_lifecycle",
            &format!(
                "phase={phase} source={} session={} request_id={}",
                self.source.as_str(),
                self.session_id,
                self.request_id
            ),
        );
        if outcome.is_err() && self.supervisor.is_running() {
            self.supervisor.shutdown();
        }
        if let Ok(mut active) = self.active_session.lock() {
            *active = None;
        }
    }

    /// Watchdog revocation (#149): send the cancel and wait for the sidecar's
    /// acknowledgement; on a missing/invalid ack the sidecar is restarted
    /// (shut down; the next start respawns). The caller re-arms only after this
    /// returns, because every outcome means the old operation can no longer run.
    pub fn revoke_acked(self, timeout: Duration) -> engine_supervisor::RevocationOutcome {
        let outcome = self.supervisor.revoke_with_ack(&self.frame, timeout);
        crate::telemetry::log_message(
            "cancel_lifecycle",
            &format!(
                "phase=revocation_{} source={} session={} request_id={}",
                match &outcome {
                    engine_supervisor::RevocationOutcome::Acked => "acked",
                    engine_supervisor::RevocationOutcome::NotRunning => "not_running",
                    engine_supervisor::RevocationOutcome::Failed { .. } => "failed_restarted",
                },
                self.source.as_str(),
                self.session_id,
                self.request_id
            ),
        );
        if let Ok(mut active) = self.active_session.lock() {
            *active = None;
        }
        outcome
    }
}

impl PreparedStart {
    pub fn without_command(state: AppState) -> Self {
        Self {
            state,
            command: None,
        }
    }

    pub fn with_command(command: PreparedEngineStart) -> Self {
        Self {
            state: AppState::Armed,
            command: Some(command),
        }
    }

    pub fn send(self) -> Result<PreparedStartResult, PreparedStartError> {
        let Some(command) = self.command else {
            return Ok(PreparedStartResult::Immediate(self.state));
        };
        // The recording start does not wait for the provider-key gate: the keys only matter for
        // cloud features at finalization, and the worker's warm-up from the shortcut press is
        // bounded (a start held behind the gate could arrive after it expired and lose the spoken
        // prefix). The keys are delivered after the start frame is accepted (below).
        let start_result =
            command
                .supervisor
                .start_without_keys()
                .map_err(|message| PreparedStartError {
                    session_id: command.session_id.clone(),
                    message,
                })?;
        if engine_supervisor::needs_snapshot_push(
            &start_result,
            command.generation_seen,
            command.supervisor.generation(),
        ) {
            for frame in &command.snapshot_frames {
                let pushed = command
                    .supervisor
                    .call_without_keys(frame, Duration::from_secs(5))
                    .map_err(|message| PreparedStartError {
                        session_id: command.session_id.clone(),
                        message,
                    })?;
                if let Some(error) = pushed.error {
                    return Err(PreparedStartError {
                        session_id: command.session_id,
                        message: error.message,
                    });
                }
            }
        }
        let recording_generation = command.supervisor.generation();
        let response = command
            .supervisor
            .call_without_keys(&command.frame, Duration::from_secs(5))
            .map_err(|message| PreparedStartError {
                session_id: command.session_id.clone(),
                message,
            })?;
        if let Some(error) = response.error {
            return Err(PreparedStartError {
                session_id: command.session_id,
                message: error.message,
            });
        }
        if !matches!(
            response.payload,
            Some(EnginePayload::RecordingStarted { .. })
        ) {
            return Err(PreparedStartError {
                session_id: command.session_id,
                message: "unexpected sidecar recording-start response".to_string(),
            });
        }
        // The engine accepted the start: capture is under way and this call returns now. The
        // provider-key delivery claim is taken HERE, synchronously, so the gate has the delivery
        // in flight before this call returns; the delivery itself (up to 3 s) runs on a
        // background thread. The stop's finalize waits on exactly this claim
        // (`EngineSupervisor::send_stop`, bounded), so a very quick stop cannot get ahead of the
        // key thread and the cloud route never depends on scheduler order. The keys gate only
        // provider (AI cleanup) processing in the engine: a slow, failed or uncertain delivery
        // never delays, fails or cancels the recording (local polish; the generation is retired
        // by the NEXT start, never mid-recording and by no key-delivery path while a capture is
        // active or reserved).
        {
            let claim = command.supervisor.claim_provider_keys_for_start();
            let slot = Arc::new(StdMutex::new(Some(claim)));
            let thread_slot = Arc::clone(&slot);
            let supervisor = Arc::clone(&command.supervisor);
            let session_id = command.session_id.clone();
            let spawned = std::thread::Builder::new()
                .name("provider-keys".to_string())
                .spawn(move || {
                    let claim = thread_slot.lock().ok().and_then(|mut held| held.take());
                    if let Some(claim) = claim {
                        if let Err(message) = supervisor.deliver_claimed_provider_keys(claim) {
                            crate::telemetry::log_message(
                                "provider_keys_background_failed",
                                &format!("session={session_id} reason={message}"),
                            );
                        }
                    }
                });
            if let Err(error) = spawned {
                // No thread: deliver here so the claim is never left in flight. The recording is
                // untouched either way.
                crate::telemetry::log_message("provider_keys_thread_failed", &error.to_string());
                let claim = slot.lock().ok().and_then(|mut held| held.take());
                if let Some(claim) = claim {
                    let _ = command.supervisor.deliver_claimed_provider_keys(claim);
                }
            }
        }
        // A restart during the start RPC cannot authorize streaming for either
        // process. Keep the cell empty; ordinary final/history recovery still runs.
        if command.supervisor.generation() == recording_generation {
            if let Ok(mut generation) = command.streaming_generation.lock() {
                *generation = Some(recording_generation);
            }
        }
        command
            .supervisor
            .note_recording_started(&command.session_id);
        Ok(PreparedStartResult::Engine {
            session_id: command.session_id,
        })
    }
}

pub struct PreparedStartError {
    pub session_id: String,
    pub message: String,
}

pub enum PreparedStartResult {
    Immediate(AppState),
    Engine { session_id: String },
}
