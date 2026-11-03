// Shared imports live on the module root (runtime.rs); `use super::*` keeps
// this file on the same namespace the include!-based layout provided.
#[allow(unused_imports)]
use super::*;

pub fn engine_state_snapshot_is_transcribing_session(state: &str, session_id: &str) -> bool {
    state == format!("Transcribing {{ session_id: {session_id:?} }}")
}

pub fn sidecar_file_transcribe_timeout(path: &std::path::Path) -> Duration {
    let size_mb = std::fs::metadata(path)
        .map(|m| m.len() / (1024 * 1024))
        .unwrap_or(0);
    let timeout_secs = size_mb
        .saturating_mul(2)
        .saturating_add(SIDECAR_FILE_TRANSCRIBE_TIMEOUT_MIN_SECS)
        .clamp(
            SIDECAR_FILE_TRANSCRIBE_TIMEOUT_MIN_SECS,
            SIDECAR_FILE_TRANSCRIBE_TIMEOUT_MAX_SECS,
        );
    Duration::from_secs(timeout_secs)
}

#[derive(Debug, PartialEq, Eq)]
pub enum SidecarControlTail {
    Deliver,
    Reset,
}

pub fn apply_sidecar_control_tail(
    transcript: &mut String,
    want_enter: &mut bool,
) -> SidecarControlTail {
    let Some(command) = rightkit_text_polish::text_pipeline::parse_control_command(transcript)
    else {
        return SidecarControlTail::Deliver;
    };
    use rightkit_text_polish::text_pipeline::ControlIntent;
    match command.intent {
        ControlIntent::Cancel => SidecarControlTail::Reset,
        ControlIntent::Stop | ControlIntent::Send => {
            *transcript = command.clean_text;
            if matches!(command.intent, ControlIntent::Send) {
                *want_enter = true;
            }
            SidecarControlTail::Deliver
        }
    }
}

#[doc(hidden)]
pub fn apply_sidecar_control_tail_for_test(input: &str) -> (bool, String, bool) {
    let mut transcript = input.to_string();
    let mut want_enter = false;
    let action = apply_sidecar_control_tail(&mut transcript, &mut want_enter);
    (
        action == SidecarControlTail::Deliver,
        transcript,
        want_enter,
    )
}
