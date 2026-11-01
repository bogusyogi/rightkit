// rightkit-dictation-runtime consumed by the exact registry pin in Cargo.toml, outside the
// workspace. Public API only; no engine process, no model, no socket.
use rightkit_dictation_runtime::crash_capture::configure_crash_identity;
use rightkit_dictation_runtime::speech_service::{parse_role, role_for};

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
        role_for(true) != role_for(false),
        "headless and shell engines take different host roles",
    );
    all &= check(
        parse_role("definitely-not-a-role").is_none(),
        "unknown role names are refused",
    );
    all &= check(
        configure_crash_identity("NoteWell", "1.0.0")
            && !configure_crash_identity("Other", "2.0.0"),
        "crash identity is configured once",
    );
    if !all {
        std::process::exit(1);
    }
}
