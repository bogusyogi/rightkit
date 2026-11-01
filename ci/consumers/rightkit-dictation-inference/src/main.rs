// rightkit-dictation-inference consumed by the exact registry pin in Cargo.toml, outside the
// workspace. Public API only; no model, no audio, no network.
use rightkit_dictation_inference::beam_emit_reward;
use rightkit_dictation_inference::file_transcribe::{
    configure_app_display_name, permission_error, E_PERMISSION,
};

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
    let reward = beam_emit_reward();
    all &= check(
        reward.is_finite() && reward >= 0.0,
        "beam emission reward is a finite non-negative default",
    );
    configure_app_display_name("NoteWell");
    let message = permission_error(std::path::Path::new("clip.wav"));
    all &= check(
        message.starts_with(E_PERMISSION) && message.contains("NoteWell"),
        "permission error is typed and names the app",
    );
    if !all {
        std::process::exit(1);
    }
}
