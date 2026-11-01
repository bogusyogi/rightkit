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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rk-doctor-hd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn digest_is_prefixed_sha256() {
        assert_eq!(
            digest_bytes(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn counts_and_newest_walk_one_level() {
        let root = scratch("walk");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::write(root.join("a").join("SKILL.md"), "x").unwrap();
        std::fs::write(root.join("note.md"), "x").unwrap();
        assert_eq!(count_entries_with(&root, "SKILL.md"), 1);
        assert_eq!(count_files_with_extension(&root, "md"), 1);
        std::fs::write(root.join("b").join("trace.jsonl"), "x").unwrap();
        let (path, _) = newest_named_file(std::slice::from_ref(&root), &["trace.jsonl"]).unwrap();
        assert!(path.ends_with("b/trace.jsonl") || path.ends_with("b\\trace.jsonl"));
        assert!(newest_named_file(std::slice::from_ref(&root), &["absent.jsonl"]).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn config_dir_prefers_the_variable_and_ignores_relative_values_when_asked() {
        let home = Path::new("home");
        assert_eq!(
            config_dir("RIGHTKIT_DOCTOR_UNSET_VAR", home, ".cfg"),
            home.join(".cfg")
        );
        assert_eq!(
            absolute_config_dir("RIGHTKIT_DOCTOR_UNSET_VAR", home, ".cfg"),
            home.join(".cfg")
        );
        assert!(absolute_root(Path::new("x")).is_absolute());
    }

    #[test]
    fn read_json_is_none_for_garbage() {
        let root = scratch("json");
        std::fs::write(root.join("a.json"), "{ not json").unwrap();
        std::fs::write(root.join("b.json"), "{\"k\":1}").unwrap();
        assert!(read_json(&root.join("a.json")).is_none());
        assert_eq!(read_json(&root.join("b.json")).unwrap()["k"], 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
