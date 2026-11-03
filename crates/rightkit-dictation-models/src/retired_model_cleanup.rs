//! One-time reclaim of the RETIRED Parakeet Unified model directory.
//!
//! Unified was retired on 2026-07-19 (TDT won 6.85% vs 7.02% WER on the
//! canonical productivity-dictation corpus, at 2.5x the decode speed and 100 MB
//! less disk). Installs that onboarded before that carry a ~500 MB Unified
//! directory that nothing will ever load again.
//!
//! This is deliberately NOT wired into the uninstaller: uninstallers frequently
//! do not run at all, and reclaiming disk as the user leaves is the least useful
//! moment to do it. Instead we reclaim on launch, but only once it is provably
//! safe — TDT must already be installed, so a failure here can never leave the
//! user with no working model.
//!
//! Guarded by a marker file so a user who deliberately re-installs Unified via
//! the Settings rollback lane does not get it deleted out from under them on the
//! next launch.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Marker written after a successful (or unnecessary) reclaim. Presence means
/// "we have already done this once; never touch Unified again".
const MARKER: &str = "unified-retired-reclaimed";
#[cfg(not(target_os = "macos"))]
const WINDOWS_TDT_MARKER: &str = "windows-tdt-retired-reclaimed-20261022";
/// Reclaim markers of earlier switches; removed once this one completes.
#[cfg(not(target_os = "macos"))]
const WINDOWS_TDT_PREVIOUS_RECLAIM_MARKERS: [&str; 2] = [
    "windows-tdt-retired-reclaimed-20260722",
    "windows-tdt-legacy-reclaimed-20260720",
];
#[cfg(target_os = "macos")]
const MACOS_TDT_MARKER: &str = "macos-tdt-retired-reclaimed-20261022";

static REPLACEMENT_DOWNLOAD_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn request_replacement_download() {
    REPLACEMENT_DOWNLOAD_REQUESTED.store(true, Ordering::SeqCst);
}

/// Consumed by the Settings renderer after it is visible. Keeping the request
/// natively means a newly-created macOS hub cannot miss the navigate event
/// while its webview is still mounting.
pub fn take_replacement_download_request() -> bool {
    REPLACEMENT_DOWNLOAD_REQUESTED.swap(false, Ordering::SeqCst)
}

#[cfg(target_os = "macos")]
const UNIFIED_SUBDIR: &str = "parakeet-unified-en-0.6b";
#[cfg(not(target_os = "macos"))]
const UNIFIED_SUBDIR: &str = "unified_static15_b128_sym_bits4_timestamp_hybrid";

fn marker_path() -> PathBuf {
    crate::app_paths::app_data_file(MARKER)
}

#[cfg(not(target_os = "macos"))]
fn windows_tdt_marker_path() -> PathBuf {
    crate::app_paths::app_data_file(WINDOWS_TDT_MARKER)
}

#[cfg(not(target_os = "macos"))]
fn retired_windows_tdt_dirs(models_root: &Path) -> Vec<(PathBuf, &'static str)> {
    crate::model_paths::RETIRED_TDT_MODELS
        .iter()
        .map(|(subdir, _)| (models_root.join(subdir), *subdir))
        .collect()
}

/// Parakeet Ultra is mandatory for any Windows install that carries a retired
/// TDT revision. A historical activation marker cannot satisfy this gate; the
/// Ultra directory and the Ultra activation marker must agree.
#[cfg(not(target_os = "macos"))]
pub fn windows_tdt_replacement_pending_for_root(models_root: &Path) -> bool {
    let retired_ready = retired_windows_tdt_dirs(models_root)
        .iter()
        .any(|(dir, subdir)| crate::model_paths::local_asr_ready(dir, subdir));
    let current = models_root.join(crate::model_paths::WINDOWS_TDT_CURRENT_SUBDIR);
    let current_activated = crate::model_paths::local_asr_ready(
        &current,
        crate::model_paths::WINDOWS_TDT_CURRENT_SUBDIR,
    ) && models_root
        .join(crate::model_paths::WINDOWS_TDT_ACTIVATED_MARKER)
        .is_file();
    retired_ready && !current_activated
}

/// macOS: a complete retired CoreML bundle under the app-data models root.
/// Ultra's own state is the shared store's activation, checked by the caller.
pub fn macos_tdt_replacement_pending_for_root(models_root: &Path) -> bool {
    crate::model_paths::local_asr_ready(
        &models_root.join(crate::model_paths::MACOS_TDT_PREVIOUS_SUBDIR),
        crate::model_paths::MACOS_TDT_PREVIOUS_SUBDIR,
    )
}

/// True when a retired TDT model (app-data or shared store) is still the model
/// in use and a verified Ultra is on offer. False while Ultra cannot be
/// downloaded (placeholder checksum, or a catalog that does not offer it), so
/// no prompt appears for a download that would fail closed.
pub fn tdt_replacement_pending() -> bool {
    if !crate::shared::ultra_downloadable() {
        return false;
    }
    if crate::shared::resolve(crate::shared::legacy_subdir()).is_some() {
        return false;
    }
    let models = crate::model_paths::install_base();
    #[cfg(not(target_os = "macos"))]
    let local = windows_tdt_replacement_pending_for_root(&models);
    #[cfg(target_os = "macos")]
    let local = macos_tdt_replacement_pending_for_root(&models);
    local || crate::shared::retired_subdir_in_store().is_some()
}

/// Roll back a transaction that was interrupted before the new activation
/// marker was durable. This runs before any engine resolution at launch, so a
/// stale `.activating` file can never select an unverified revision forever.
#[cfg(not(target_os = "macos"))]
pub fn recover_interrupted_windows_tdt_activation_for_root(models_root: &Path) -> bool {
    let activating = models_root.join(crate::model_paths::WINDOWS_TDT_ACTIVATING_MARKER);
    if !activating.is_file() {
        return false;
    }
    let activated = models_root.join(crate::model_paths::WINDOWS_TDT_ACTIVATED_MARKER);
    let _ = std::fs::remove_file(&activating);
    if !activated.is_file() {
        let current = models_root.join(crate::model_paths::WINDOWS_TDT_CURRENT_SUBDIR);
        // A compatibility junction may point at a model used by other apps.
        // Old private activation metadata never authorizes shared deletion.
        if !current.is_symlink() {
            let _ = std::fs::remove_dir_all(current);
        }
    }
    true
}

#[cfg(not(target_os = "macos"))]
pub fn recover_interrupted_windows_tdt_activation() -> bool {
    let models = crate::model_paths::install_base();
    let recovered = recover_interrupted_windows_tdt_activation_for_root(&models);
    if recovered {
        crate::telemetry::log_message(
            "windows_tdt_activation_recovered",
            "cleared interrupted Parakeet Ultra activation",
        );
    }
    recovered
}

/// Remove every retired Windows TDT directory under `models_root` only after
/// Ultra is complete and activated by the app-data transaction (activated
/// marker). Path-scoped; leaves the current directory untouched on every
/// failure path.
#[cfg(not(target_os = "macos"))]
pub fn reclaim_superseded_windows_tdt_for_root(models_root: &Path) -> Option<u64> {
    let current = models_root.join(crate::model_paths::WINDOWS_TDT_CURRENT_SUBDIR);
    if !crate::model_paths::local_asr_ready(
        &current,
        crate::model_paths::WINDOWS_TDT_CURRENT_SUBDIR,
    ) || !models_root
        .join(crate::model_paths::WINDOWS_TDT_ACTIVATED_MARKER)
        .is_file()
    {
        return None;
    }
    remove_retired_windows_tdt_dirs(models_root)
}

/// Deletion half of the reclaim; callers have already proven Ultra active
/// (activated marker, or the shared store's verified activation).
#[cfg(not(target_os = "macos"))]
fn remove_retired_windows_tdt_dirs(models_root: &Path) -> Option<u64> {
    let mut freed = 0u64;
    let mut removed_any = false;
    let mut failed = false;
    for (retired, _) in retired_windows_tdt_dirs(models_root) {
        if !retired.is_dir() {
            continue;
        }
        let bytes = dir_size(&retired);
        match std::fs::remove_dir_all(&retired) {
            Ok(()) => {
                freed = freed.saturating_add(bytes);
                removed_any = true;
            }
            Err(error) => {
                failed = true;
                crate::telemetry::log_message(
                    "windows_tdt_retired_reclaim_failed",
                    &format!("{}: {error}", retired.display()),
                );
            }
        }
    }
    (!failed && removed_any).then_some(freed)
}

#[cfg(not(target_os = "macos"))]
pub fn reclaim_superseded_windows_tdt() -> Option<u64> {
    let marker = windows_tdt_marker_path();
    if marker.exists() {
        return None;
    }
    let models = crate::model_paths::install_base();
    // The shared store's activation (native load of the pinned Ultra entry
    // under the idle guard) proves Ultra just as the app-data marker does. Only a prepare that
    // verified it in THIS process counts: the persisted activation record alone never deletes.
    let result = if crate::shared::is_verified_active() {
        remove_retired_windows_tdt_dirs(&models)
    } else {
        reclaim_superseded_windows_tdt_for_root(&models)
    };
    let retired_absent = retired_windows_tdt_dirs(&models)
        .iter()
        .all(|(dir, _)| !dir.exists());
    if retired_absent {
        let marker_value = result
            .map(|freed| format!("freed {freed}\n"))
            .unwrap_or_else(|| "absent\n".to_string());
        let _ = rightkit_fs::atomic_write(&marker, marker_value.as_bytes());
        for old_marker in [
            crate::model_paths::WINDOWS_TDT_PREVIOUS_ACTIVATED_MARKER,
            crate::model_paths::WINDOWS_TDT_PREVIOUS_ACTIVATING_MARKER,
            crate::model_paths::WINDOWS_TDT_OLDER_ACTIVATED_MARKER,
            crate::model_paths::WINDOWS_TDT_OLDER_ACTIVATING_MARKER,
        ] {
            let _ = std::fs::remove_file(models.join(old_marker));
        }
        for old_reclaim_marker in WINDOWS_TDT_PREVIOUS_RECLAIM_MARKERS {
            let _ = std::fs::remove_file(crate::app_paths::app_data_file(old_reclaim_marker));
        }
        if let Some(freed) = result {
            crate::telemetry::log_message(
                "windows_tdt_retired_reclaimed",
                &format!("removed retired Windows TDT revisions ({freed} bytes)"),
            );
        }
    }
    result
}

/// macOS reclaim of the retired CoreML TDT v3 bundle under app-data. Runs only
/// after Ultra is proven (`ultra_verified`: the shared store activated it).
/// Never follows a symlink and leaves the bundle on every failure path.
pub fn reclaim_superseded_macos_tdt_for_root(
    models_root: &Path,
    ultra_verified: bool,
) -> Option<u64> {
    if !ultra_verified {
        return None;
    }
    let retired = models_root.join(crate::model_paths::MACOS_TDT_PREVIOUS_SUBDIR);
    if retired.is_symlink() || !retired.is_dir() {
        return None;
    }
    let bytes = dir_size(&retired);
    match std::fs::remove_dir_all(&retired) {
        Ok(()) => Some(bytes),
        Err(error) => {
            crate::telemetry::log_message(
                "macos_tdt_retired_reclaim_failed",
                &format!("{}: {error}", retired.display()),
            );
            None
        }
    }
}

#[cfg(target_os = "macos")]
pub fn reclaim_superseded_macos_tdt() -> Option<u64> {
    let marker = crate::app_paths::app_data_file(MACOS_TDT_MARKER);
    if marker.exists() {
        return None;
    }
    // Verified in this process by `prepare`; never the persisted record alone.
    let verified = crate::shared::is_verified_active();
    let models = crate::model_paths::install_base();
    let result = reclaim_superseded_macos_tdt_for_root(&models, verified);
    // Marker only once the retired directory is verifiably gone after Ultra.
    if verified
        && !models
            .join(crate::model_paths::MACOS_TDT_PREVIOUS_SUBDIR)
            .exists()
    {
        let value = result
            .map(|freed| format!("freed {freed}\n"))
            .unwrap_or_else(|| "absent\n".to_string());
        let _ = rightkit_fs::atomic_write(&marker, value.as_bytes());
        if let Some(freed) = result {
            crate::telemetry::log_message(
                "macos_tdt_retired_reclaimed",
                &format!("removed retired CoreML TDT v3 ({freed} bytes)"),
            );
        }
    }
    result
}

/// Leftovers a retired model leaves in the app-data models folder once its directory is gone: the
/// zero-byte `<subdir>.operation.lock` and the `.<subdir>.symlink-aside` link an older install swap
/// left behind (seen on a Mac that moved to Ultra, pointing at the retired CoreML bundle). Only the
/// link itself is removed, never what it points at, and nothing is touched while the retired
/// directory still exists. Idempotent; returns how many entries it removed.
pub fn remove_retired_model_leftovers_for_root(models_root: &Path, retired: &[&str]) -> usize {
    let mut removed = 0;
    for subdir in retired {
        if models_root.join(subdir).symlink_metadata().is_ok() {
            continue;
        }
        let lock = models_root.join(format!("{subdir}.operation.lock"));
        if lock.symlink_metadata().is_ok_and(|meta| meta.is_file())
            && std::fs::remove_file(&lock).is_ok()
        {
            removed += 1;
        }
        let aside = models_root.join(format!(".{subdir}.symlink-aside"));
        if aside
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_symlink())
            && remove_link(&aside).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// Remove a symlink without following it (a directory link on Windows is removed as a directory).
fn remove_link(link: &Path) -> std::io::Result<()> {
    std::fs::remove_file(link).or_else(|_| std::fs::remove_dir(link))
}

/// [`remove_retired_model_leftovers_for_root`] for this install: every retired TDT revision and
/// the retired Unified model.
pub fn remove_retired_model_leftovers() -> usize {
    let mut retired: Vec<&str> = crate::model_paths::RETIRED_TDT_MODELS
        .iter()
        .map(|(subdir, _)| *subdir)
        .collect();
    retired.push(UNIFIED_SUBDIR);
    let removed =
        remove_retired_model_leftovers_for_root(&crate::model_paths::install_base(), &retired);
    if removed > 0 {
        crate::telemetry::log_message(
            "retired_model_leftovers_removed",
            &format!("entries={removed}"),
        );
    }
    removed
}

/// Platform dispatcher for the post-activation reclaim of retired TDT models.
pub fn reclaim_superseded_tdt() -> Option<u64> {
    #[cfg(not(target_os = "macos"))]
    {
        reclaim_superseded_windows_tdt()
    }
    #[cfg(target_os = "macos")]
    {
        reclaim_superseded_macos_tdt()
    }
}

/// Whether the TDT model is installed and resolvable (path selection: the settings layer keeps a
/// retired-Unified selection running until this is true). It may rest on the persisted activation
/// record; the reclaim below uses [`tdt_installed_verified`] instead.
pub fn tdt_installed() -> bool {
    let subdir = crate::model_paths::subdir_for_backend("parakeet-tdt");
    crate::model_paths::resolve_model_dir(subdir).is_some()
}

/// [`tdt_installed`] for gates that delete or finalize: the shared copy counts only once a
/// prepare verified it in this process, never on the persisted activation record alone (the
/// record is written by an earlier launch, and the copy may have rotted since).
pub fn tdt_installed_verified() -> bool {
    let subdir = crate::model_paths::subdir_for_backend("parakeet-tdt");
    crate::model_paths::resolve_model_dir_verified(subdir).is_some()
}

/// True when this install is still carrying the retired Unified model as its
/// selected backend (i.e. an update from a pre-2026-07-19 build that has not
/// yet downloaded TDT). Drives the in-app "model replaced" notice.
pub fn unified_replacement_pending() -> bool {
    crate::settings::snapshot_asr_backend_raw().as_deref() == Some("parakeet-unified")
        && !tdt_installed()
}

pub fn replacement_pending() -> bool {
    unified_replacement_pending() || tdt_replacement_pending()
}

/// Bytes freed by the last reclaim, for the caller to surface in the UI. `None`
/// means nothing was reclaimed (already done, not present, or not yet safe).
pub fn reclaim_retired_unified() -> Option<u64> {
    if marker_path().exists() {
        return None;
    }

    // Never delete the only working model: TDT must be resolvable first, and a shared Ultra copy
    // counts only once it was verified in this process (the launch reclaim thread runs before the
    // boot prepare does).
    if !tdt_installed_verified() {
        return None;
    }

    // TDT is present — a persisted retired-Unified selection can flip safely
    // now (covers installs that already had both models on disk and therefore
    // never pass through the download-complete hook).
    if let Err(e) = crate::settings::finalize_retired_unified_switch() {
        crate::telemetry::log_message("retired_unified_switch_error", &e);
        return None; // retry next launch; don't delete while still selected
    }

    let Some(unified) = crate::model_paths::resolve_model_dir(UNIFIED_SUBDIR) else {
        // Nothing to reclaim — mark so we stop checking on every launch.
        let _ = rightkit_fs::atomic_write(&marker_path(), b"absent\n");
        return None;
    };

    let freed = dir_size(&unified);
    match std::fs::remove_dir_all(&unified) {
        Ok(()) => {
            let _ =
                rightkit_fs::atomic_write(&marker_path(), format!("freed {freed}\n").as_bytes());
            crate::telemetry::log_message(
                "retired_unified_reclaimed",
                &format!("removed {} ({freed} bytes)", unified.display()),
            );
            Some(freed)
        }
        Err(e) => {
            // Leave the marker absent so a later launch can retry (e.g. the dir
            // was locked by a still-shutting-down engine process).
            crate::telemetry::log_message(
                "retired_unified_reclaim_failed",
                &format!("{}: {e}", unified.display()),
            );
            None
        }
    }
}

fn dir_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}
