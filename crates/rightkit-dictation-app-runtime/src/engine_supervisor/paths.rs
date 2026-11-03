// Sidecar / ffmpeg path resolution + engine-binary identity telemetry.

#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;

/// Path resolution: the sidecar binary lives next to the shell binary in the installed build
/// (or in the macOS bundle's `Resources`). No environment variable and no source-checkout path
/// redirects it, so the shipped app cannot be pointed at an arbitrary binary that would inherit
/// its TCC grants.
pub fn resolve_engine_bin() -> Option<std::path::PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sibling = parent.join(engine_bin_name());
            if sibling.exists() {
                return Some(sibling);
            }
            #[cfg(target_os = "macos")]
            if let Some(contents_dir) = parent.parent() {
                let resource_bin = contents_dir.join("Resources").join(engine_bin_name());
                if resource_bin.exists() {
                    return Some(resource_bin);
                }
            }
        }
    }
    None
}

use crate::host::engine_bin_name;

// Model-subdir topology (which on-disk folder each backend alias maps to) is
// owned by `crate::model_paths::subdir_for_backend` — the ONE source of truth.
// The supervisor is process lifecycle, not model topology, and must not grow
// its own duplicate resolver.

pub(crate) fn binary_identity_suffix(path: &PathBuf) -> String {
    let Ok(metadata) = std::fs::metadata(path) else {
        return "bytes=<unavailable> modified=<unavailable>".to_string();
    };
    let modified = metadata
        .modified()
        .map(|mtime| format!("{mtime:?}"))
        .unwrap_or_else(|_| "<unavailable>".to_string());
    format!("bytes={} modified={modified}", metadata.len())
}

/// Keep ANE cache misses attributable without moving Apple's cache. The
/// sidecar binary key is persisted in app-data, while cache presence is read
/// from Apple's normal per-user location on macOS. This distinguishes an
/// expected cache purge from a binary change that necessarily invalidates it.
pub(crate) fn engine_cache_telemetry(path: &PathBuf, app_data_root: &std::path::Path) -> String {
    let key = engine_binary_key(path).unwrap_or_else(|| "unavailable".to_string());
    let marker = app_data_root.join("engine-binary-key.json");
    let previous_key = std::fs::read_to_string(&marker).ok().and_then(|value| {
        value
            .split_once("\"key\":\"")
            .and_then(|(_, suffix)| suffix.split_once('"').map(|(key, _)| key.to_string()))
    });
    let cause = match previous_key.as_deref() {
        None => "baseline",
        Some(previous) if previous != key => "binary_key_change",
        Some(_) if ane_cache_has_bundle() => "cache_warm",
        Some(_) => "cache_purge",
    };

    if key != "unavailable" {
        let marker_json = format!("{{\"key\":\"{key}\"}}\n");
        let options = rightkit_fs::atomic::AtomicWriteOptions {
            create_parents: true,
            ..Default::default()
        };
        let _ = rightkit_fs::atomic::atomic_write_with_options(&marker, options, |file| {
            std::io::Write::write_all(file, marker_json.as_bytes())
        });
    }
    format!("engine_key={key} ane_cache_cause={cause}")
}

fn engine_binary_key(path: &PathBuf) -> Option<String> {
    // FNV-1a is deliberately local and dependency-free; this is an identity
    // key for telemetry, not a signature or release integrity assertion.
    let bytes = std::fs::read(path).ok()?;
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Some(format!("{hash:016x}"))
}

#[cfg(target_os = "macos")]
fn ane_cache_has_bundle() -> bool {
    // The account's home from the account database, never $HOME (release switch rule).
    let Ok(home) = rightkit_fs::app_dirs::account_home() else {
        return false;
    };
    let bundle = crate::host::engine_host().bundle_identifier;
    !bundle.is_empty()
        && has_compiled_bundle(
            &home
                .join("Library/Caches")
                .join(bundle)
                .join("com.apple.e5rt.e5bundlecache"),
            0,
        )
}

#[cfg(not(target_os = "macos"))]
fn ane_cache_has_bundle() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn has_compiled_bundle(root: &Path, depth: usize) -> bool {
    if depth > 6 {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        (path.is_dir() && name.ends_with(".bundle") && !name.contains(".tmp."))
            || (path.is_dir() && has_compiled_bundle(&path, depth + 1))
    })
}
