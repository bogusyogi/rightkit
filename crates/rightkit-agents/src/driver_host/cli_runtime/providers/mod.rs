//! Per-vendor adapter implementations.
//!
//! PR 1 of the architecture plan only declares the module surface.
//! Each per-vendor file lands its real implementation in its own PR
//! (Claude/Codex migrate with PR 6; Grok, Cursor, Cline, and Command
//! Code ship with PRs 8–11).
//!
//! Keeping the directory present from PR 1 ensures adapter code
//! added later can land in a predictable location without a later
//! refactor.

pub mod claude;
pub mod cline;
pub mod codex;
pub mod commandcode;
pub mod cursor;
pub mod grok;

fn executable_sha256(path: Option<&std::path::Path>) -> Option<String> {
    path.and_then(|path| {
        crate::driver_host::cli_runtime::recovery::compute_executable_digest(path).ok()
    })
}
