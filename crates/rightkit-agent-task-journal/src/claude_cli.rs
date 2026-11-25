//! Helpers for running the person's own Claude Code command-line tool as a bounded, tools-off
//! (or tool-restricted) agent run. Pure `std`: where the executable may be, whether a launch
//! preset's arguments still turn every tool off, the `PATH` a script install needs, and
//! the stale-scratch sweep.
//!
//! The process itself is `rightkit-llm`'s `CliProvider`; this module only decides things that do
//! not need it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Whether the last `--disallowedTools` in `all` is followed by `*`.
pub fn has_disallow_all<S: AsRef<str>>(all: &[S]) -> bool {
    all.iter()
        .rposition(|a| a.as_ref() == "--disallowedTools")
        .is_some_and(|i| all.get(i + 1).is_some_and(|next| next.as_ref() == "*"))
}

/// Whether launch arguments still turn every built-in tool off and skip every MCP server. The last
/// `--tools` must be followed by an empty argument, the last `--disallowedTools` by `*`, and
/// `--strict-mcp-config` must be present; nothing may add tools, an MCP config, a directory or a
/// permission bypass. Fails closed: a preset change that drops one of them is `false`.
pub fn tools_are_off_args(all: &[&str]) -> bool {
    let tools_empty = all
        .iter()
        .rposition(|a| *a == "--tools")
        .is_some_and(|i| all.get(i + 1).is_some_and(|next| next.is_empty()));
    let forbidden = [
        "--mcp-config",
        "--allowedTools",
        "--allowed-tools",
        "--dangerously-skip-permissions",
        "--permission-mode",
        "--add-dir",
    ];
    tools_empty
        && has_disallow_all(all)
        && all.contains(&"--strict-mcp-config")
        && !all.iter().any(|a| forbidden.contains(a))
}

/// The places the Claude Code executable is usually installed, in lookup order, and the file names
/// to look for on `PATH`. `appdata` is the `APPDATA` folder (used on Windows only).
pub fn claude_candidates(
    home: &Path,
    appdata: Option<&OsStr>,
) -> (Vec<PathBuf>, &'static [&'static str]) {
    #[cfg(not(windows))]
    {
        let _ = appdata;
        (
            vec![
                home.join(".local/bin/claude"),
                home.join(".claude/local/claude"),
                PathBuf::from("/opt/homebrew/bin/claude"),
                PathBuf::from("/usr/local/bin/claude"),
                home.join(".npm-global/bin/claude"),
                home.join(".bun/bin/claude"),
            ],
            &["claude"],
        )
    }
    #[cfg(windows)]
    {
        let mut list = vec![home.join(r".local\bin\claude.exe")];
        if let Some(appdata) = appdata {
            let npm = PathBuf::from(appdata).join("npm");
            list.push(npm.join("claude.exe"));
            list.push(npm.join("claude.cmd"));
        }
        (list, &["claude.exe", "claude.cmd"])
    }
}

/// Whether `path` is a regular file (and, on Unix, has an execute bit).
pub fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The first of `candidates` that is an executable file.
pub fn first_executable(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|p| is_executable_file(p))
}

/// The program's folder, the inherited `PATH`, then the usual tool folders, for a script install
/// that starts `node` through `env` (a launchd or Task Scheduler `PATH` is minimal). `None` off
/// Unix or when a folder cannot be joined.
pub fn widened_path(program: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(parent) = program.parent() {
            dirs.push(parent.to_path_buf());
        }
        if let Some(inherited) = std::env::var_os("PATH") {
            dirs.extend(std::env::split_paths(&inherited));
        }
        for fixed in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
            dirs.push(PathBuf::from(fixed));
        }
        std::env::join_paths(dirs)
            .ok()
            .map(|joined| joined.to_string_lossy().into_owned())
    }
    #[cfg(not(unix))]
    {
        let _ = program;
        None
    }
}

/// Removes `run-*` folders in `root` older than an hour (left by a run that died). Never touches a
/// fresh one.
pub fn sweep_stale_runs(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs() > 3600);
        if old && entry.file_name().to_string_lossy().starts_with("run-") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}
