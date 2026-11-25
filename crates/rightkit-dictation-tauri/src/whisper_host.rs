//! The `WhisperHost` for a desktop shell: the application's three services (the engine binary
//! lookup, the Pro check and the signed entitlement token) and an optional progress emitter, plus
//! the two command bodies over the Whisper model flow in `rightkit-dictation-models`.
//!
//! How a progress payload reaches the renderer (the event name and the emit call) is the
//! application's, passed as the `emit` closure. The Tauri command entry points and the move onto a
//! blocking thread stay in the application.

use std::path::PathBuf;

use rightkit_dictation_models::whisper_model::{
    download_whisper_model_blocking, whisper_model_status, WhisperHost, WhisperModelStatus,
};
use serde::Serialize;

/// One progress report of a Whisper model download or install, as the renderer receives it.
#[derive(Serialize)]
pub struct WhisperProgress<'a> {
    /// Phase name reported by the model flow.
    pub phase: &'a str,
    /// Bytes received so far.
    pub received: u64,
    /// Total bytes, when the flow knows it.
    pub total: u64,
    /// Human-readable status text from the flow.
    pub message: &'a str,
}

/// The application services the model flow reads.
#[derive(Clone, Copy)]
pub struct WhisperServices {
    /// Path of the engine binary, when it can be resolved.
    pub resolve_engine_bin: fn() -> Option<PathBuf>,
    /// Whether the Pro entitlement is active.
    pub has_pro: fn() -> bool,
    /// The server-signed entitlement token the gated download presents.
    pub signed_entitlement_token: fn() -> Option<String>,
}

struct EmittingWhisperHost<E> {
    services: WhisperServices,
    emit: Option<E>,
}

impl<E> WhisperHost for EmittingWhisperHost<E>
where
    E: Fn(&WhisperProgress<'_>) + Send + Sync,
{
    fn resolve_engine_bin(&self) -> Option<PathBuf> {
        (self.services.resolve_engine_bin)()
    }

    fn has_pro(&self) -> bool {
        (self.services.has_pro)()
    }

    fn signed_entitlement_token(&self) -> Option<String> {
        (self.services.signed_entitlement_token)()
    }

    fn emit_progress(&self, phase: &str, received: u64, total: u64, message: &str) {
        if let Some(emit) = self.emit.as_ref() {
            emit(&WhisperProgress {
                phase,
                received,
                total,
                message,
            });
        }
    }
}

/// The Whisper model status. Reports no progress.
pub fn whisper_model_status_for(services: WhisperServices) -> WhisperModelStatus {
    let host = EmittingWhisperHost::<fn(&WhisperProgress<'_>)> {
        services,
        emit: None,
    };
    whisper_model_status(&host)
}

/// Download and install the Whisper model, blocking the calling thread; every progress report goes
/// to `emit`. Run it on a blocking thread.
pub fn download_whisper_model_for<E>(services: WhisperServices, emit: E) -> Result<(), String>
where
    E: Fn(&WhisperProgress<'_>) + Send + Sync,
{
    let host = EmittingWhisperHost {
        services,
        emit: Some(emit),
    };
    download_whisper_model_blocking(&host)
}
