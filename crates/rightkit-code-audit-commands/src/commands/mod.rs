//! Audit, assurance, coverage, minimize, report, review, rule and verify
//! command logic: argument structs and handlers that return JSON results or a
//! typed [`CommandError`]. The host CLI registers the commands and prints the
//! results. Shared command support comes from `rightkit-evidence-commands`.

pub mod audit;
pub mod audit_assurance;
pub mod audit_runtime;
pub mod coverage;
pub mod minimize;
pub mod report;
pub mod review;
pub mod rules;
pub mod verify;

pub use rightkit_evidence_commands::commands::{
    audit_inventory_source, audit_inventory_source_scoped, audit_signing_key, command_host,
    display_path, io_error, native_application_for, program_name, set_command_host, CommandError,
    CommandHost, CommandResult, CommonArgs,
};

use std::path::PathBuf;
use std::sync::OnceLock;

static INSTALLED_SHARE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Declare the installed release's `share/<product>` directory, where the
/// shipped provider registry (`assets/registry/providers.json`) and minimize
/// policy live. The first call wins; it returns `false` when already set.
pub fn set_installed_share_dir(dir: PathBuf) -> bool {
    INSTALLED_SHARE_DIR.set(dir).is_ok()
}

/// The installed share directory, when the host declared one.
pub fn installed_share_dir() -> Option<PathBuf> {
    INSTALLED_SHARE_DIR.get().cloned()
}

/// PATH lookup helpers shared by the coverage command.
mod doctor {
    use std::path::PathBuf;

    /// First executable named `command` on `PATH`.
    pub(super) fn command_path(command: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(command);
            if candidate.is_file() {
                return Some(candidate);
            }
            #[cfg(windows)]
            for extension in [".exe", ".cmd", ".bat"] {
                let candidate = directory.join(format!("{command}{extension}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }
}
