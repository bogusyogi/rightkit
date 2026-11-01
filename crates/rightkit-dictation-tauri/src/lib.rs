//! Dictation runtime for a Tauri shell: the recording state machine over the engine sidecar,
//! history sequencing, the transcribe watchdog and start-sequence liveness, with every application
//! and platform service reached through [`host::DictationHost`].
//!
//! Install a host with [`host::install_host`] before starting dictation.

#![allow(
    clippy::result_large_err,
    clippy::large_enum_variant,
    clippy::too_many_arguments,
    clippy::doc_lazy_continuation,
    clippy::manual_clamp,
    dead_code,
    unused_imports
)]

pub mod host;
pub mod runtime;
pub mod runtime_lock;

pub mod dictation_error;
pub mod engine_event_consumer;
pub mod start_sequence;

pub use rightkit_dictation_app_runtime::{engine_process, engine_supervisor, sidecar_log};
pub use rightkit_dictation_core::{controller, engine, state};
pub use rightkit_dictation_telemetry::telemetry;

/// Delivery records and the OS delivery entry points.
pub mod delivery {
    pub use rightkit_dictation_command_host::delivery::*;
}

/// History records and the store contract.
pub mod history {
    pub use rightkit_transcript_library::history::*;
}

/// Settings the runtime reads from the host.
pub mod settings {
    use crate::host::host;
    use rightkit_dictation_core::settings::SettingsBlob;
    use rightkit_transcript_library::history::HistoryRetention;

    pub(crate) fn snapshot() -> SettingsBlob {
        host().settings_snapshot()
    }
    pub(crate) fn onboarding_complete() -> bool {
        host().onboarding_complete()
    }
    pub(crate) fn effective_asr_backend(stored: &str, is_pro: bool) -> String {
        host().effective_asr_backend(stored, is_pro)
    }
    pub(crate) fn history_retention() -> HistoryRetention {
        host().history_retention()
    }
}

pub mod license {
    pub(crate) fn engine_is_pro() -> bool {
        crate::host::host().engine_is_pro()
    }
}

pub mod vocabulary {
    use crate::host::host;
    use rightkit_text_polish::vocab_learn::Suggestion;
    use rightkit_text_polish::vocabulary::VocabularyEntry;

    pub(crate) fn list() -> Vec<VocabularyEntry> {
        host().vocabulary_list()
    }
    pub(crate) fn suggest_all(candidates: Vec<Suggestion>) -> Result<(), String> {
        host().suggest_vocabulary(candidates)
    }
}

pub mod cloud_sync {
    use crate::host::host;

    pub(crate) fn notify_local_change() {
        host().notify_local_change();
    }
    pub(crate) fn enqueue_deleted_ids(ids: &[String]) -> Result<(), String> {
        host().enqueue_deleted_ids(ids)
    }
}

pub mod media_control {
    use crate::host::host;

    pub(crate) fn begin_for_session(session_id: &str) {
        host().media_begin_for_session(session_id);
    }
    pub(crate) fn restore_for_session(session_id: &str, reason: &str) {
        host().media_restore_for_session(session_id, reason);
    }
    pub(crate) fn restore_any(reason: &str) {
        host().media_restore_any(reason);
    }
}

pub mod streaming_insertion {
    use crate::host::host;

    pub(crate) fn take_for_final(session_id: &str, transcript: &str) -> Option<String> {
        host().streaming_take_for_final(session_id, transcript)
    }
    pub(crate) fn press_enter() -> bool {
        host().streaming_press_enter()
    }
    pub(crate) fn erase_on_cancel(session_id: &str) {
        host().streaming_erase_on_cancel(session_id);
    }
}

pub mod pill_dispatch_queue {
    pub(crate) fn assert_not_locked() {
        crate::host::host().assert_ui_dispatch_unlocked();
    }
}
