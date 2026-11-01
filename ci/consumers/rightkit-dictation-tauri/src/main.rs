// rightkit-dictation-tauri consumed by the exact registry pin in Cargo.toml, outside the workspace.
// Public API only; no engine process, no audio device.
use std::sync::Arc;

use rightkit_dictation_core::settings::SettingsBlob;
use rightkit_dictation_tauri::dictation_error::label_for_error;
use rightkit_dictation_tauri::host::{install_host, DictationHost};
use rightkit_dictation_tauri::start_sequence::{
    capture_liveness, note_capture_live, CaptureLiveness,
};
use rightkit_transcript_library::history::HistoryStore;

struct Host;

impl DictationHost for Host {
    fn settings_snapshot(&self) -> SettingsBlob {
        SettingsBlob::default()
    }
    fn onboarding_complete(&self) -> bool {
        true
    }
    fn open_json_history(&self) -> Result<Box<dyn HistoryStore>, String> {
        Err("not used".into())
    }
    fn open_encrypted_history(&self) -> Result<Box<dyn HistoryStore>, String> {
        Err("not used".into())
    }
}

fn check(ok: bool, what: &str) -> bool {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
    }
    ok
}

fn main() {
    let mut all = true;
    all &= check(install_host(Arc::new(Host)), "host installs once");
    all &= check(!install_host(Arc::new(Host)), "a second install is refused");
    all &= check(
        label_for_error("engine E_QUALITY_GATE: repetition_loop") == "No clear speech",
        "quality errors map to a pill label",
    );
    note_capture_live("consumer-session");
    all &= check(
        capture_liveness("consumer-session") == CaptureLiveness::Live,
        "capture liveness is recorded per session",
    );
    if !all {
        std::process::exit(1);
    }
}
