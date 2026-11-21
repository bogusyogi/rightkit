// rightkit-dictation-tauri 0.3.2 consumed by exact registry pin (`=0.3.2`), outside the workspace.
//
// Proves, through the public API only, the pure user-facing error vocabulary and the capture
// wording:
// - dictation_error::label_for_error maps an unknown message to "Error" and an explicit mute
//   message to the mute label, and is_explicit_microphone_mute_error recognises the mute text;
// - start_sequence::capture_not_live_message names the window in milliseconds;
// - start_sequence::CAPTURE_LIVE_WINDOW is the four-second window.
// The start sequence, runtime lock, engine supervisor and Tauri host are not run: they need a
// live microphone, the engine process and the desktop shell. No I/O, no network, no process spawn.
// Exits 1 on the first failed check.
use std::{path::Path, time::Duration};

use rightkit_dictation_tauri::dictation_error::{
    is_explicit_microphone_mute_error, label_for_error,
};
use rightkit_dictation_tauri::start_sequence::{capture_not_live_message, CAPTURE_LIVE_WINDOW};
use rightkit_dictation_tauri::{
    asr_compat, capture_folder, command_outcome, hotkey_gates, renderer_events, settings_policy,
};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let _read_wav: fn(&Path) -> Result<f64, String> = asr_compat::wav_duration_secs;
    let _prepare_capture_folder = capture_folder::prepare_root;
    let _drag_drop_payload = renderer_events::native_drag_drop_payload;
    check(
        command_outcome::classify("key_sequence", Some("ctrl+z"))
            == command_outcome::CommandOutcome::Undo,
        "command outcome classifies undo",
    );
    check(
        !hotkey_gates::dictation_hotkeys_enabled(true, false, false),
        "launch blocks dictation hotkeys",
    );
    check(
        settings_policy::choice_or_default(Some("invalid"), &["auto"], "auto") == "auto",
        "unsupported settings value uses the default",
    );
    check(
        label_for_error("zzz") == "Error",
        "an unrecognised message is labelled Error",
    );
    check(
        is_explicit_microphone_mute_error("Microphone muted"),
        "an explicit mute message is recognised",
    );
    check(
        label_for_error("Microphone muted") == "Microphone muted. Unmute it and try again.",
        "an explicit mute message gets the mute label",
    );
    check(
        capture_not_live_message(Duration::from_millis(4_000)).contains("4000 ms"),
        "the not-live message names the window in milliseconds",
    );
    check(
        CAPTURE_LIVE_WINDOW == Duration::from_millis(4_000),
        "the capture-live window is four seconds",
    );
    println!("OK rightkit-dictation-tauri 0.3.2 (registry consumer)");
}
