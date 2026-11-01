// ASR runtime for the sidecar.
//
// The sidecar loads the ASR model once and runs authoritative decode on every
// dictation stop. Long static Parakeet clips use padded-window decode so 15 s
// model limits do not become hard text boundaries.

mod models;
mod runtime;
#[cfg(test)]
mod tests;
mod windowing;

pub use models::{apply_probe_context_bias, apply_utterance_bias, models_base};
pub use runtime::{
    append_scheduled_static15_text, backend_supports_scheduled_static15, route_ep_mismatch,
    scheduled_static15_ready_segment, AsrEp, AsrRuntime, BackgroundDecodeKind, FileJob, FileStep,
    ScheduledStatic15, AUDIBLE_BLANK_TRANSCRIPTION_ERROR,
};
pub use windowing::{confident_static15_terminal_cut, Static15TerminalCut};

#[cfg(target_os = "macos")]
pub(crate) use windowing::transcribe_padded_window_text;
