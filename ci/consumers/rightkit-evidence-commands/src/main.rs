// rightkit-evidence-commands consumed by the exact registry pin, outside the workspace.
// Proves: stable command exit codes, the neutral command host, and verbatim-prefix stripping.
// No command is executed.
use rightkit_evidence_commands::commands::{command_host, display_path, CommandError};
use std::path::{Path, PathBuf};

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
        CommandError::usage("x").code == 4 && CommandError::policy("x").code == 1,
        "stable exit codes",
    );
    check(
        CommandError::integrity("x").code == 5 && CommandError::incomplete("x").code == 2,
        "integrity and incomplete codes",
    );
    check(
        command_host().program == "rightkit" && command_host().workspace_dir == ".rightkit",
        "neutral command host",
    );
    check(
        display_path(Path::new(r"\\?\C:\repo")) == PathBuf::from(r"C:\repo"),
        "verbatim prefix stripped",
    );
}
