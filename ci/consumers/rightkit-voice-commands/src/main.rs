// rightkit-voice-commands consumed by the exact registry pin in Cargo.toml, outside the workspace.
// Public API only; in-memory values, no OS side effects.
use rightkit_voice_commands::command_recognition::{command_catalog, recognize_command};
use rightkit_voice_commands::{is_allowed_external_url, CommandAction};

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
    all &= check(
        matches!(
            recognize_command("select all"),
            Some(CommandAction::KeySequence { ref chords, .. }) if chords.len() == 1 && chords[0] == "ctrl+a"
        ),
        "\"select all\" recognizes as ctrl+a",
    );
    all &= check(
        recognize_command("I think we should meet tomorrow afternoon").is_none(),
        "ordinary dictation is not a command",
    );
    all &= check(
        !command_catalog().is_empty(),
        "command catalog is non-empty",
    );
    let allowed = ["https://example.app/", "mailto:hi@example.app"];
    all &= check(
        is_allowed_external_url("https://example.app/", &allowed)
            && !is_allowed_external_url("https://example.app/x", &allowed)
            && !is_allowed_external_url(" https://example.app/", &allowed),
        "external link allowlist is exact",
    );
    if !all {
        std::process::exit(1);
    }
}
