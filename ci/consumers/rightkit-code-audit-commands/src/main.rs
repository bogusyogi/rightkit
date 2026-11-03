// rightkit-code-audit-commands 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Proves, through the public API only, the install-directory declaration and the command error
// vocabulary:
// - commands::set_installed_share_dir accepts the first declaration and refuses a second one
//   (first call wins), without reading the directory;
// - CommandError::usage carries exit code 4 and its message, and io_error maps to internal (3).
// The audit, coverage, rules and assurance handlers are not run: they read repositories and
// provider registries from disk. No network, no child process. Exits 1 on the first failed check.
use std::path::PathBuf;

use rightkit_code_audit_commands::commands::{set_installed_share_dir, CommandError};

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
        set_installed_share_dir(PathBuf::from("/nonexistent/rightkit-share")),
        "the first share directory declaration is accepted",
    );
    check(
        !set_installed_share_dir(PathBuf::from("/nonexistent/other")),
        "a second share directory declaration is refused",
    );
    let usage = CommandError::usage("bad flag");
    check(
        usage.code == 4 && usage.message == "bad flag",
        "usage errors carry exit code 4 and their message",
    );
    let internal = CommandError::internal("disk full");
    check(
        internal.code == 3 && internal.message == "disk full",
        "internal errors carry exit code 3 and their message",
    );
    println!("OK rightkit-code-audit-commands 0.1.0 (registry consumer)");
}
