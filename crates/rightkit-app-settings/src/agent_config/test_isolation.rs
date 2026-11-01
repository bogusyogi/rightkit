//! Fail-closed state-root isolation for tests and QA harnesses (`test-mode` builds only).
//!
//! A `test-mode` binary honours `RIGHTKIT_SUITE_ROOT` (the app root is then
//! `<suite root>/apps/the host app`) and, when it is NOT set, falls back to the real per-user root.
//! Every harness that spawns a the host app binary, or runs in-process code that calls
//! [`crate::agent_config::state_root()`], goes through this module so that fallback can never happen silently:
//! each entry point asserts the suite root is a fresh directory inside a temp dir and refuses
//! anything that overlaps the real `Orthic Labs` data folder.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The environment variable a `test-mode` build reads (rightkit-daemon's `ENV_SUITE_ROOT`).
pub const SUITE_ROOT_ENV: &str = rightkit_daemon::ENV_SUITE_ROOT;

/// Canonical form even when `path` does not exist yet: canonicalize the nearest existing
/// ancestor and re-append the rest, so both sides of a prefix test use the same spelling.
fn normalize(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut probe = path;
    loop {
        if let Ok(real) = std::fs::canonicalize(probe) {
            return tail.iter().rev().fold(real, |acc, part| acc.join(part));
        }
        match (probe.parent(), probe.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                probe = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn is_under(path: &Path, base: &Path) -> bool {
    let (path, base) = (normalize(path), normalize(base));
    path.starts_with(&base) && path != base
}

/// Panic unless `suite_root` is strictly inside the system temp dir or one of `extra_allowed`
/// (for example Cargo's `CARGO_TARGET_TMPDIR`), is not empty or relative, and does not overlap
/// the real per-user data folder.
pub fn assert_isolated(suite_root: &Path, extra_allowed: &[&Path]) {
    assert!(
        suite_root.is_absolute(),
        "test suite root must be absolute, got {suite_root:?}"
    );
    let mut allowed = vec![std::env::temp_dir()];
    allowed.extend(extra_allowed.iter().map(|path| path.to_path_buf()));
    assert!(
        allowed.iter().any(|base| is_under(suite_root, base)),
        "test suite root {suite_root:?} must be inside a temp dir ({allowed:?}); refusing to run \
         against anything that could be the real Host data folder"
    );
    let real = dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .map(|dir| normalize(&dir.join("Orthic Labs")));
    if let Some(real) = real {
        let candidate = normalize(suite_root);
        assert!(
            !candidate.starts_with(&real) && !real.starts_with(&candidate),
            "test suite root {suite_root:?} overlaps the real data folder {real:?}"
        );
    }
}

/// The the host app app root a `test-mode` process uses for `RIGHTKIT_SUITE_ROOT=<suite_root>`:
/// `<suite_root>/apps/the host app`. Resolved by the same code the product runs, and created.
pub fn app_root_in(suite_root: &Path) -> PathBuf {
    let suite = rightkit_daemon::Suite::at(suite_root)
        .unwrap_or_else(|error| panic!("test suite root {suite_root:?} is unusable: {error}"));
    suite
        .app_root(crate::agent_config::state_root::APP_NAME)
        .unwrap_or_else(|error| panic!("cannot open the Agent app root in {suite_root:?}: {error}"))
        .root()
        .to_path_buf()
}

/// Point a child process at an isolated suite root. Asserts the root first; returns the app
/// root so the caller can seed config or read state. Call this for EVERY spawned the host app
/// binary (CLI, daemon, TUI, MCP, QA).
pub fn apply_to_command(
    command: &mut Command,
    suite_root: &Path,
    extra_allowed: &[&Path],
) -> PathBuf {
    assert_isolated(suite_root, extra_allowed);
    command.env(SUITE_ROOT_ENV, suite_root);
    app_root_in(suite_root)
}

/// Panic unless `command` has `RIGHTKIT_SUITE_ROOT` set to a non-empty path inside a temp dir.
/// For harnesses that build a `Command` in one place and spawn it in another.
pub fn assert_command_isolated(command: &Command, extra_allowed: &[&Path]) {
    let value = command
        .get_envs()
        .find(|(key, _)| *key == SUITE_ROOT_ENV)
        .and_then(|(_, value)| value)
        .unwrap_or_else(|| panic!("{SUITE_ROOT_ENV} is not set on {command:?}; a test-mode binary would use the real data folder"));
    assert!(
        !value.is_empty(),
        "{SUITE_ROOT_ENV} is empty on {command:?}"
    );
    assert_isolated(Path::new(value), extra_allowed);
}

/// In-process variant: set `RIGHTKIT_SUITE_ROOT` for this process after asserting it. The caller
/// restores the previous value (tests already serialize environment changes). Returns the app root.
pub fn set_process_suite_root(suite_root: &Path) -> PathBuf {
    assert_isolated(suite_root, &[]);
    // SAFETY: tests serialize process-environment changes with their own env lock.
    #[allow(unused_unsafe)]
    unsafe {
        std::env::set_var(SUITE_ROOT_ENV, suite_root)
    };
    app_root_in(suite_root)
}

/// Restore a value captured with `std::env::var_os(SUITE_ROOT_ENV)` before the test.
pub fn restore_process_suite_root(previous: Option<std::ffi::OsString>) {
    // SAFETY: as for `set_process_suite_root`.
    #[allow(unused_unsafe)]
    unsafe {
        match previous {
            Some(value) => std::env::set_var(SUITE_ROOT_ENV, value),
            None => std::env::remove_var(SUITE_ROOT_ENV),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_fresh_dir_inside_temp_and_resolves_the_app_root() {
        let dir = tempfile::tempdir().unwrap();
        assert_isolated(dir.path(), &[]);
        let app = app_root_in(dir.path());
        assert!(app.ends_with(Path::new("apps").join(crate::agent_config::state_root::APP_NAME)));
        assert!(app.is_dir());
    }

    #[test]
    #[should_panic(expected = "must be inside a temp dir")]
    fn refuses_a_dir_outside_temp() {
        let outside = dirs::home_dir()
            .expect("home dir")
            .join("definitely-not-temp");
        assert_isolated(&outside, &[]);
    }

    #[test]
    #[should_panic(expected = "must be absolute")]
    fn refuses_a_relative_root() {
        assert_isolated(Path::new("relative/root"), &[]);
    }

    #[test]
    #[should_panic(expected = "is not set")]
    fn refuses_a_command_with_no_suite_root() {
        let command = Command::new("rightkit-agent");
        assert_command_isolated(&command, &[]);
    }

    #[test]
    fn accepts_a_command_with_an_isolated_suite_root() {
        let dir = tempfile::tempdir().unwrap();
        let mut command = Command::new("rightkit-agent");
        let app = apply_to_command(&mut command, dir.path(), &[]);
        assert!(app.starts_with(normalize(dir.path())) || app.starts_with(dir.path()));
        assert_command_isolated(&command, &[]);
    }
}
