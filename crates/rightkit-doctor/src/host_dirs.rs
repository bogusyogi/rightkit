//! Agent-host configuration folders and small file helpers for install probes.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

/// The user's home folder (`USERPROFILE`, then `HOME`; `.` when neither is set).
pub fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// A configuration folder: the value of `env_var`, else `home/default_relative`.
pub fn config_dir(env_var: &str, home: &Path, default_relative: &str) -> PathBuf {
    std::env::var_os(env_var)
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(default_relative))
}

/// Like [`config_dir`], but a relative value of `env_var` is ignored.
pub fn absolute_config_dir(env_var: &str, home: &Path, default_relative: &str) -> PathBuf {
    std::env::var_os(env_var)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(default_relative))
}

/// `path` made absolute against the current folder and lexically cleaned, without requiring it
/// to exist.
pub fn absolute_root(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    joined.components().collect()
}

/// `path` parsed as JSON; `None` when it cannot be read or parsed.
pub fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// `sha256:<hex>` of `bytes`.
pub fn digest_bytes(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        rightkit_crypto::hex_encode(&rightkit_crypto::sha256(bytes))
    )
}

/// How many entries of `directory` contain a file named `marker`.
pub fn count_entries_with(directory: &Path, marker: &str) -> usize {
    std::fs::read_dir(directory)
        .ok()
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path().join(marker).is_file())
                .count()
        })
        .unwrap_or(0)
}

/// How many files in `directory` have the extension `extension` (without the dot).
pub fn count_files_with_extension(directory: &Path, extension: &str) -> usize {
    std::fs::read_dir(directory)
        .ok()
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.path().extension().and_then(|e| e.to_str()) == Some(extension)
                })
                .count()
        })
        .unwrap_or(0)
}

/// The most recently modified file called one of `names` directly in one of `dirs` or in a
/// folder one level below them.
pub fn newest_named_file(dirs: &[PathBuf], names: &[&str]) -> Option<(PathBuf, SystemTime)> {
    let mut newest: Option<(PathBuf, SystemTime)> = None;
    let mut consider = |path: PathBuf| {
        let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
            return;
        };
        if newest.as_ref().is_none_or(|(_, t)| modified > *t) {
            newest = Some((path, modified));
        }
    };
    for dir in dirs {
        for name in names {
            consider(dir.join(name));
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.filter_map(Result::ok) {
                if entry.path().is_dir() {
                    for name in names {
                        consider(entry.path().join(name));
                    }
                }
            }
        }
    }
    newest
}
