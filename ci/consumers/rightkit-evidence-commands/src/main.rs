// rightkit-evidence-commands 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Proves, through the public API only, the path display rule and the error vocabulary:
// - display_path strips the Windows extended-length prefix and leaves other paths unchanged;
// - CommandError::usage carries exit code 4 and io_error maps to internal (exit code 3).
// Command handlers that need an installed command host or read the native application
// configuration from the environment are not called. No I/O, no network, no process spawn.
// Exits 1 on the first failed check.
use std::path::{Path, PathBuf};

use rightkit_evidence_commands::commands::{display_path, io_error, CommandError};

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
        display_path(Path::new(r"\\?\C:\repo")) == PathBuf::from(r"C:\repo"),
        "the extended-length prefix is removed for display",
    );
    check(
        display_path(Path::new("/plain/path")) == PathBuf::from("/plain/path"),
        "an ordinary path is shown unchanged",
    );
    let usage = CommandError::usage("bad flag");
    check(
        usage.code == 4 && usage.message == "bad flag",
        "usage errors carry exit code 4 and their message",
    );
    let io = io_error("disk full");
    check(
        io.code == 3 && io.message == "disk full",
        "io errors map to internal exit code 3",
    );
    println!("OK rightkit-evidence-commands 0.1.0 (registry consumer)");
}
