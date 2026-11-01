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

pub enum RuntimeApp {
    Fake(DictationController<crate::host::HostDelivery, crate::host::HostHistory>),
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
        let sent = command.supervisor.send(&command.frame);
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
        let start_result = command
            .supervisor
            .start()
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
                    .call(frame, Duration::from_secs(5))
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
        let response = command
            .supervisor
            .call(&command.frame, Duration::from_secs(5))
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
