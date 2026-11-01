//! ENG-04: launch-time model verification policy (test-mode switch `RIGHTKIT_MODEL_VERIFY`).
//!
//! `full` (default, shipped): every launch runs `store::ensure_ready`, which SHA-256s every pinned
//! file (Windows 7 files / 363 MB, macOS 23 files / 433 MB) under the shell's `runtime_lock`.
//!
//! `receipt`: after one full verification, record a stat receipt of every pinned file (size,
//! mtime and, where the platform has them, device/inode/ctime or creation time). A later launch
//! whose files still match the receipt exactly skips the hashing and only registers this app's
//! store reference, as `ensure_ready`'s verified branch does. Any mismatch runs the full
//! verification again and rewrites the receipt. Skipped on a receipt hit: the store's retired-entry
//! prune, which the next full verification runs.
//!
//! A release build never reads the switch, so it always verifies in full.

use std::path::Path;

use serde_json::{json, Value};

use crate::shared::store;

const RECEIPT_FILE: &str = "shared-model-verify-receipt.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerifyPolicy {
    Full,
    Receipt,
}

pub fn parse(raw: Option<&str>) -> VerifyPolicy {
    match raw.map(str::trim) {
        Some("receipt") => VerifyPolicy::Receipt,
        _ => VerifyPolicy::Full,
    }
}

pub fn policy() -> VerifyPolicy {
    parse(
        rightkit_dictation_core::dev_var!("RIGHTKIT_MODEL_VERIFY")
            .ok()
            .as_deref(),
    )
}

/// The stat fingerprint of `spec`'s pinned files in `dir`; `None` when any file is unreadable.
pub fn fingerprint(dir: &Path, spec: &store::ModelSpec) -> Option<Value> {
    let mut files = Vec::with_capacity(spec.files.len());
    for file in spec.files {
        let meta = std::fs::metadata(dir.join(file.path)).ok()?;
        if !meta.is_file() {
            return None;
        }
        let mtime_ns = meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        files.push(json!({
            "path": file.path,
            "len": meta.len(),
            "mtime_ns": mtime_ns.to_string(),
            "identity": identity(&meta),
        }));
    }
    Some(json!({
        "schema": 1,
        "id": spec.id,
        "version": spec.version,
        "dir": dir.to_string_lossy(),
        "files": files,
    }))
}

#[cfg(unix)]
fn identity(meta: &std::fs::Metadata) -> Value {
    use std::os::unix::fs::MetadataExt;
    json!([meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()])
}

#[cfg(windows)]
fn identity(meta: &std::fs::Metadata) -> Value {
    use std::os::windows::fs::MetadataExt;
    json!([meta.creation_time()])
}

#[cfg(not(any(unix, windows)))]
fn identity(_meta: &std::fs::Metadata) -> Value {
    Value::Null
}

fn receipt_path() -> std::path::PathBuf {
    crate::app_paths::app_data_file(RECEIPT_FILE)
}

/// True when the recorded receipt matches `dir` exactly.
pub fn matches(dir: &Path, spec: &store::ModelSpec) -> bool {
    let Some(current) = fingerprint(dir, spec) else {
        return false;
    };
    std::fs::read(receipt_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|recorded| recorded == current)
}

/// Record the receipt after a successful full verification. Best effort: a missing receipt only
/// means the next launch verifies in full.
pub fn record(dir: &Path, spec: &store::ModelSpec) {
    let Some(current) = fingerprint(dir, spec) else {
        return;
    };
    let path = receipt_path();
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .map_err(|error| error.to_string())
        .and_then(|()| serde_json::to_vec(&current).map_err(|error| error.to_string()))
        .and_then(|bytes| {
            rightkit_fs::atomic_write(&path, &bytes).map_err(|error| error.to_string())
        });
    if let Err(error) = written {
        crate::telemetry::log_message("shared_model_verify_receipt_write_failed", &error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_full() {
        assert_eq!(parse(None), VerifyPolicy::Full);
        assert_eq!(parse(Some("full")), VerifyPolicy::Full);
        assert_eq!(parse(Some("junk")), VerifyPolicy::Full);
        assert_eq!(parse(Some("receipt")), VerifyPolicy::Receipt);
    }

    #[test]
    fn fingerprint_tracks_size_and_presence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = crate::ultra_model::spec();
        assert!(
            fingerprint(dir.path(), &spec).is_none(),
            "missing files never match"
        );
        for file in spec.files {
            let path = dir.path().join(file.path);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, b"x").expect("write");
        }
        let first = fingerprint(dir.path(), &spec).expect("all files present");
        assert_eq!(fingerprint(dir.path(), &spec), Some(first.clone()));
        let changed = dir.path().join(spec.files[0].path);
        std::fs::write(&changed, b"xy").expect("rewrite");
        assert_ne!(fingerprint(dir.path(), &spec), Some(first));
    }
}
