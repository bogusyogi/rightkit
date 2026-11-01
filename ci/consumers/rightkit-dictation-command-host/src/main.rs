// rightkit-dictation-command-host consumed by the exact registry pin in Cargo.toml, outside the
// workspace. Public API only; no OS side effects.
use rightkit_dictation_command_host::host::{configure, identity, is_own_process, HostIdentity};

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
        !is_own_process("notewell.exe"),
        "nothing is own before configure",
    );
    configure(HostIdentity {
        bundle_id: "com.example.notewell",
        process_name_prefix: "NoteWell",
        overlay_window_titles: &["NoteWell Pill"],
        display_name: "NoteWell",
    });
    all &= check(
        is_own_process("notewell-engine.exe") && !is_own_process("notepad.exe"),
        "host prefix matches case-insensitively",
    );
    all &= check(identity().display_name == "NoteWell", "identity reads back");
    if !all {
        std::process::exit(1);
    }
}
