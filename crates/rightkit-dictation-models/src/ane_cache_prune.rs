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
//!
//! Every delete is confined to that verified cache directory (`confined_remove`): the cache
//! path is opened one component at a time without following a link, each level below it is
//! opened through the handle of the level above, and a removal is a handle-relative delete. A
//! link at any level is never entered; the bundle identifier that names the directory is
//! validated before it is joined into a path ([`is_valid_bundle_identifier`]).

#[cfg(target_os = "macos")]
const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(300);

/// Apple's own name for the compile-cache folder inside an app's cache directory.
#[cfg(target_os = "macos")]
const E5_CACHE_DIR: &str = "com.apple.e5rt.e5bundlecache";

static BUNDLE_IDENTIFIER: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// Longest bundle identifier [`is_valid_bundle_identifier`] accepts.
pub const MAX_BUNDLE_IDENTIFIER_LEN: usize = 128;

/// True when `identifier` is safe to use as one directory name under the user cache directory:
/// 1 to [`MAX_BUNDLE_IDENTIFIER_LEN`] bytes of ASCII letters, digits, `.`, `-` and `_`, not
/// starting or ending with `.`, with no `..`. A separator, a drive prefix, a `..` component, a
/// control character or non-ASCII text can therefore never reach a path the cache helpers
/// delete from.
pub fn is_valid_bundle_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier.len() <= MAX_BUNDLE_IDENTIFIER_LEN
        && !identifier.starts_with('.')
        && !identifier.ends_with('.')
        && !identifier.contains("..")
        && identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// Name the app's bundle identifier (the ANE compile-cache namespace under the user cache dir).
/// Call once at startup; until then the cache helpers touch nothing under the user cache.
/// Returns false, and configures nothing, when one was already set or when `identifier` is not
/// valid ([`is_valid_bundle_identifier`]): the cache helpers then stay inert.
pub fn configure_bundle_identifier(identifier: &'static str) -> bool {
    is_valid_bundle_identifier(identifier) && BUNDLE_IDENTIFIER.set(identifier).is_ok()
}

#[cfg(target_os = "macos")]
fn bundle_identifier() -> Option<&'static str> {
    BUNDLE_IDENTIFIER.get().copied()
}

/// Remove orphaned ANE compile-cache temp bundles left by earlier launches.
/// macOS-only; a no-op elsewhere. Safe to call from a background thread at
/// startup — it never blocks the launch and only touches this app's own cache.
///
/// Everything is deleted through pinned directory handles (`confined_remove`): a link at
/// the cache path or at any level below it makes that part of the cache skipped, never entered.
#[cfg(target_os = "macos")]
pub fn prune_orphaned_ane_temps() {
    let (Some(cache_root), Some(bundle_identifier)) = (dirs::cache_dir(), bundle_identifier())
    else {
        return;
    };
    match prune_staging_bundles(&cache_root, bundle_identifier) {
        Ok((removed, freed)) if removed > 0 => {
            crate::telemetry::log_message(
                "ane_cache_temps_pruned",
                &format!("removed={removed} freed_mb={}", freed / (1024 * 1024)),
            );
        }
        Ok(_) => {}
        Err(reason) => crate::telemetry::log_message("ane_cache_prune_skipped", reason),
    }
}

/// Remove the stale staging bundles under `<cache_root>/<bundle_identifier>/<E5_CACHE_DIR>`;
/// returns how many bundles went and the bytes freed. `bundle_identifier` was validated by
/// [`configure_bundle_identifier`].
#[cfg(target_os = "macos")]
fn prune_staging_bundles(
    cache_root: &std::path::Path,
    bundle_identifier: &str,
) -> Result<(usize, u64), &'static str> {
    use crate::confined_remove::ConfinedDir;

    let mut removed = 0usize;
    let mut freed = 0u64;
    let Some(root) = ConfinedDir::open(cache_root)? else {
        return Ok((0, 0));
    };
    let Some(bundle_cache) = root.open_chain(&[bundle_identifier, E5_CACHE_DIR])? else {
        return Ok((0, 0));
    };
    // Layout: <cache>/<os-build>/<key-hash>/<content-hash>.tmp.<pid>_<inode>.bundle
    for build in bundle_cache.dir_names()? {
        let Ok(Some(build_dir)) = bundle_cache.open_child(&build) else {
            continue;
        };
        let Ok(keys) = build_dir.dir_names() else {
            continue;
        };
        for key in keys {
            let Ok(Some(key_dir)) = build_dir.open_child(&key) else {
                continue;
            };
            let Ok(names) = key_dir.dir_names() else {
                continue;
            };
            for name in names {
                // Only staging dirs: `<hash>.tmp.<pid>_<inode>.bundle`.
                if !name.contains(".tmp.") || !name.ends_with(".bundle") {
                    continue;
                }
                // `STALE_AFTER` keeps anything a live compile might still be promoting; a
                // modification time that cannot be read counts as recent.
                let Ok(report) = key_dir.remove_dir_tree(&name, Some(STALE_AFTER)) else {
                    continue;
                };
                if report.retained == 0 && report.removed > 0 {
                    removed += 1;
                    freed = freed.saturating_add(report.bytes_freed);
                }
            }
        }
    }
    Ok((removed, freed))
}

#[cfg(not(target_os = "macos"))]
pub fn prune_orphaned_ane_temps() {}

/// Result returned by the explicit developer cache-recompile action.
#[derive(Clone, Debug, serde::Serialize)]
pub struct CoreMlCacheRecompileResult {
    pub entries_removed: usize,
    pub message: String,
}

/// Remove the cache directory `root` through its pinned parent and return how many entries it
/// held at the top level. A link at `root` or in its path, or an entry that cannot be removed,
/// is an error: an explicit clear fails loudly instead of reporting success.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn clear_owned_cache_root(root: &std::path::Path) -> Result<usize, String> {
    use crate::confined_remove::ConfinedDir;

    let failed = |reason: &str| format!("Core ML cache could not be cleared: {reason}");
    let parent = root.parent().ok_or_else(|| failed("invalid location"))?;
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| failed("invalid location"))?;
    let Some(parent_dir) = ConfinedDir::open(parent).map_err(failed)? else {
        return Ok(0);
    };
    let entries = match parent_dir.open_child(name).map_err(failed)? {
        Some(dir) => dir.entry_count().unwrap_or(0),
        None => return Ok(0),
    };
    let report = parent_dir.remove_dir_tree(name, None).map_err(failed)?;
    // A child that could not be removed is retained, not an error of its own; the clear did not
    // finish.
    if report.retained > 0 {
        return Err(failed("some entries were left in place"));
    }
    Ok(entries)
}

/// Delete only the app's writable compiled-model caches. Never reaches the
/// app-data `models/` tree or packaged `.mlmodelc` bundles.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub fn clear_persistent_coreml_caches() -> Result<CoreMlCacheRecompileResult, String> {
    let compiled = crate::app_paths::app_data_file("coreml-cache");
    // The identifier was validated by `configure_bundle_identifier`.
    let app_ane = dirs::cache_dir()
        .zip(bundle_identifier())
        .map(|(root, bundle_identifier)| root.join(bundle_identifier).join(E5_CACHE_DIR));
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
