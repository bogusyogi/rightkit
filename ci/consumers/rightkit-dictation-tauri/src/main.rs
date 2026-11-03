// rightkit-dictation-tauri 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
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
use std::time::Duration;

use rightkit_dictation_tauri::dictation_error::{
    is_explicit_microphone_mute_error, label_for_error,
};
use rightkit_dictation_tauri::start_sequence::{capture_not_live_message, CAPTURE_LIVE_WINDOW};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
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
    println!("OK rightkit-dictation-tauri 0.1.0 (registry consumer)");
}
