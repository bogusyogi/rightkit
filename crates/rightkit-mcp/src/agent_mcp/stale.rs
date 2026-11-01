//! Stale user-level stdio servers (contract R4, D16).
//!
//! A user-level `mcp.json` can keep a server whose script was deleted (the Dell's
//! `harness-under-test` entry). Such a server must not be spawned every session; it is shown as
//! `unreachable: <path> missing`. Only an absolute script path can be judged here: a relative
//! one resolves against a working directory this module does not know.

use std::path::Path;

use crate::agent_mcp::types::StdioConfig;

/// Interpreters whose first non-flag argument is a script file.
const SCRIPT_RUNNERS: &[&str] = &["py", "python", "python3", "node", "nodejs"];

/// `Some("unreachable: <path> missing")` when the server's command, or the script a
/// `py`/`python`/`node` command runs, is an absolute path that does not exist.
pub fn stdio_unreachable_reason(config: &StdioConfig) -> Option<String> {
    let command = config.command.trim();
    if command.is_empty() {
        return None;
    }
    if Path::new(command).is_absolute() && !Path::new(command).exists() {
        return Some(format!("unreachable: {command} missing"));
    }
    let program = Path::new(command)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_ascii_lowercase)?;
    if !SCRIPT_RUNNERS.contains(&program.as_str()) {
        return None;
    }
    let args = config.args.iter();
    for arg in args {
        // `-m module` and `-c code` run no file; other flags (`-3.11`, `-u`) are skipped.
        if arg == "-m" || arg == "-c" || arg == "-e" || arg == "--eval" {
            return None;
        }
        if arg.starts_with('-') {
            continue;
        }
        let script = Path::new(arg);
        if script.is_absolute() && !script.exists() {
            return Some(format!("unreachable: {arg} missing"));
        }
        return None;
    }
    None
}
