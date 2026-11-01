//! Housekeeping for Apple's on-device ANE compile cache
//! (`~/Library/Caches/<bundle-id>/com.apple.e5rt.e5bundlecache`).
//!
//! When an `MLModel` is loaded with the Neural Engine, macOS's `aned` daemon
//! compiles a device-specific program, stages it in a
//! `<hash>.tmp.<pid>_<inode>.bundle` directory, and *asynchronously* promotes
//! it to the final `<hash>.bundle` name after the load call returns. If the app
//! that requested the compile exits before that promotion finishes, the staging
//! directory is orphaned — a full copy of the compiled model (the Parakeet
//! AudioEncoder is ~516 MB) that `aned` never reuses. Repeated launch/quit
//! cycles pile these up and can leave the encoder cache never finalizing, so
//! every cold launch recompiles.
//!
//! We cannot make `aned` promote faster, but this cache directory is keyed by
//! our own bundle id, so we own its housekeeping. On startup we delete only the
//! `*.tmp.*.bundle` staging directories that are clearly stale (not modified for
//! several minutes — a live promotion touches them within seconds of a ~30 s
//! compile). Finalized `<hash>.bundle` entries are never touched.

#[cfg(target_os = "macos")]
const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(300);

static BUNDLE_IDENTIFIER: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// Name the app's bundle identifier (the ANE compile-cache namespace under the user cache dir).
/// Call once at startup; until then the cache helpers touch nothing under the user cache.
/// Returns false when one was already set.
pub fn configure_bundle_identifier(identifier: &'static str) -> bool {
    BUNDLE_IDENTIFIER.set(identifier).is_ok()
}

#[cfg(target_os = "macos")]
fn bundle_identifier() -> Option<&'static str> {
    BUNDLE_IDENTIFIER.get().copied()
}

/// Remove orphaned ANE compile-cache temp bundles left by earlier launches.
/// macOS-only; a no-op elsewhere. Safe to call from a background thread at
/// startup — it never blocks the launch and only touches this app's own cache.
#[cfg(target_os = "macos")]
pub fn prune_orphaned_ane_temps() {
    use std::fs;
    use std::time::SystemTime;

    let (Some(cache_root), Some(bundle_identifier)) = (dirs::cache_dir(), bundle_identifier())
    else {
        return;
    };
    let bundle_cache = cache_root
        .join(bundle_identifier)
        .join("com.apple.e5rt.e5bundlecache");
    if !bundle_cache.exists() {
        return;
    }

    let now = SystemTime::now();
    let mut removed = 0usize;
    let mut freed: u64 = 0;

    // Layout: <cache>/<os-build>/<key-hash>/<content-hash>.tmp.<pid>_<inode>.bundle
    let Ok(build_dirs) = fs::read_dir(&bundle_cache) else {
        return;
    };
    for build in build_dirs.flatten() {
        let Ok(key_dirs) = fs::read_dir(build.path()) else {
            continue;
        };
        for key in key_dirs.flatten() {
            let Ok(entries) = fs::read_dir(key.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                // Only staging dirs: `<hash>.tmp.<pid>_<inode>.bundle`.
                if !name.contains(".tmp.") || !name.ends_with(".bundle") {
                    continue;
                }
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                // Skip anything a live compile might still be promoting.
                let recently_touched = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|mtime| now.duration_since(mtime).ok())
                    .map(|age| age < STALE_AFTER)
                    .unwrap_or(false);
                if recently_touched {
                    continue;
                }
                let size = dir_size(&path);
                if fs::remove_dir_all(&path).is_ok() {
                    removed += 1;
                    freed += size;
                }
            }
        }
    }

    if removed > 0 {
        crate::telemetry::log_message(
            "ane_cache_temps_pruned",
            &format!("removed={removed} freed_mb={}", freed / (1024 * 1024)),
        );
    }
}

#[cfg(target_os = "macos")]
fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let Ok(md) = entry.metadata() else {
                continue;
            };
            if md.is_dir() {
                total += dir_size(&entry.path());
            } else {
                total += md.len();
            }
        }
    }
    total
}

#[cfg(not(target_os = "macos"))]
pub fn prune_orphaned_ane_temps() {}

/// Result returned by the explicit developer cache-recompile action.
#[derive(Clone, Debug, serde::Serialize)]
pub struct CoreMlCacheRecompileResult {
    pub entries_removed: usize,
    pub message: String,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn clear_owned_cache_root(root: &std::path::Path) -> Result<usize, String> {
    if !root.exists() {
        return Ok(0);
    }
    let entries = std::fs::read_dir(root)
        .map_err(|error| format!("read CoreML cache {}: {error}", root.display()))?
        .count();
    std::fs::remove_dir_all(root)
        .map_err(|error| format!("clear CoreML cache {}: {error}", root.display()))?;
    Ok(entries)
}

/// Delete only the app's writable compiled-model caches. Never reaches the
/// app-data `models/` tree or packaged `.mlmodelc` bundles.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub fn clear_persistent_coreml_caches() -> Result<CoreMlCacheRecompileResult, String> {
    let compiled = crate::app_paths::app_data_file("coreml-cache");
    let app_ane = dirs::cache_dir()
        .zip(bundle_identifier())
        .map(|(root, bundle_identifier)| {
            root.join(bundle_identifier)
                .join("com.apple.e5rt.e5bundlecache")
        });
    let mut entries_removed = clear_owned_cache_root(&compiled)?;
    if let Some(cache) = app_ane {
        entries_removed += clear_owned_cache_root(&cache)?;
    }
    Ok(CoreMlCacheRecompileResult {
        entries_removed,
        message: "Core ML cache cleared.".to_string(),
    })
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
pub fn clear_persistent_coreml_caches() -> Result<CoreMlCacheRecompileResult, String> {
    Err("Core ML cache recompilation is available on Apple Silicon Macs only".to_string())
}

#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
mod tests {
    use super::clear_owned_cache_root;

    #[test]
    fn clear_owned_cache_root_removes_only_its_exact_root() {
        let root =
            std::env::temp_dir().join(format!("rightkit-coreml-cache-test-{}", std::process::id()));
        let protected = root.with_extension("models");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("compiled-artifact"), b"cache").unwrap();
        std::fs::create_dir_all(&protected).unwrap();
        std::fs::write(protected.join("model.onnx"), b"model").unwrap();
        assert_eq!(clear_owned_cache_root(&root).unwrap(), 1);
        assert!(!root.exists());
        assert!(protected.join("model.onnx").exists());
        let _ = std::fs::remove_dir_all(protected);
    }
}
